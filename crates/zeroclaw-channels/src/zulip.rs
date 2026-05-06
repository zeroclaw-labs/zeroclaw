//! Zulip channel — long-poll Events API MVP.
//!
//! Works against any Zulip-compatible server (zulipchat.com or self-hosted).
//! Authenticates via a bot account email + API key as HTTP Basic, registers
//! a long-poll event queue at listen start, and replies through
//! `messages.send`.
//!
//! # Auth
//! Bot account email + API key. Operator creates the bot in
//! **Personal settings → Bots → Add a new bot**, then copies the bot email
//! and API key. Both are sent as HTTP Basic auth on every request.
//!
//! # Inbound
//! On `listen()`:
//!
//! 1. `POST {server}/api/v1/register` with `event_types=["message"]` and an
//!    optional stream narrow. Response gives `queue_id` + initial
//!    `last_event_id`.
//! 2. Loop on `GET {server}/api/v1/events?queue_id={qid}&last_event_id={leid}`,
//!    blocking up to `event_timeout_secs`. Each `message` event is converted
//!    to a `ChannelMessage`. The cursor advances on every successful poll.
//! 3. On `BAD_EVENT_QUEUE_ID` (Zulip expired the queue), re-register and
//!    resume. Other transient errors back off and retry.
//!
//! Filters drop the bot's own messages (`sender_email == bot_email`,
//! case-insensitive), senders not on `allowed_users`, and edits.
//!
//! # Outbound
//! `POST {server}/api/v1/messages` (form-urlencoded). Recipient encoding:
//!
//! * `"stream:Stream Name"` — sends to a stream. Topic comes from
//!   `SendMessage.thread_ts` if set, otherwise `default_topic`.
//! * `"stream:Stream Name/Topic"` — explicit topic in the recipient.
//! * `"private:user@example.com,user2@example.com"` — DM (multi-party
//!   supported).
//! * Bare email — DM shorthand.
//!
//! Bodies over 8000 characters are split at sentence/word boundaries with
//! `(i/N) ` continuation markers; Zulip's documented limit is 10000, but the
//! UI starts truncating well before that.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const ZULIP_BODY_SOFT_LIMIT: usize = 8000;
const REGISTER_BACKOFF_MIN: Duration = Duration::from_secs(2);
const REGISTER_BACKOFF_MAX: Duration = Duration::from_secs(60);

pub struct ZulipChannel {
    server_url: String,
    bot_email: String,
    api_key: String,
    allowed_users: Vec<String>,
    streams: Vec<String>,
    default_topic: String,
    event_timeout: Duration,
    /// Active queue id + last seen event id. Reset on re-register.
    queue: Mutex<Option<QueueState>>,
}

#[derive(Clone)]
struct QueueState {
    queue_id: String,
    last_event_id: i64,
}

#[derive(Deserialize)]
struct RegisterResponse {
    #[serde(default)]
    result: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    queue_id: Option<String>,
    #[serde(default)]
    last_event_id: Option<i64>,
}

#[derive(Deserialize)]
struct EventsResponse {
    #[serde(default)]
    result: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    events: Vec<ZulipEvent>,
}

#[derive(Deserialize, Clone, Debug)]
struct ZulipEvent {
    id: i64,
    #[serde(rename = "type", default)]
    event_type: String,
    #[serde(default)]
    message: Option<ZulipMessage>,
    /// Set on `update_message` events; we don't process those today but want
    /// to skip them cleanly without serde failing.
    #[serde(default)]
    flags: Option<serde_json::Value>,
}

#[derive(Deserialize, Clone, Debug)]
struct ZulipMessage {
    id: i64,
    #[serde(rename = "type", default)]
    msg_type: String,
    #[serde(default)]
    sender_email: String,
    #[serde(default)]
    sender_full_name: String,
    /// `display_recipient` is a string for streams (the stream name) or an
    /// array of `{email,id,full_name}` for private messages. Capture it raw.
    #[serde(default)]
    display_recipient: serde_json::Value,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    timestamp: i64,
    /// Some Zulip versions surface a `last_edit_timestamp`; presence indicates
    /// the message has been edited since posting.
    #[serde(default)]
    last_edit_timestamp: Option<i64>,
}

#[derive(Serialize)]
struct SendForm<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    to: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<&'a str>,
    content: &'a str,
}

#[derive(Deserialize)]
struct SendResponse {
    #[serde(default)]
    result: String,
    #[serde(default)]
    msg: String,
}

impl ZulipChannel {
    pub fn new(
        server_url: String,
        bot_email: String,
        api_key: String,
        allowed_users: Vec<String>,
        streams: Vec<String>,
        default_topic: String,
        event_timeout_secs: u64,
    ) -> Self {
        Self {
            server_url: normalize_server_url(&server_url),
            bot_email,
            api_key,
            allowed_users,
            streams,
            default_topic: if default_topic.trim().is_empty() {
                "agent".to_string()
            } else {
                default_topic
            },
            event_timeout: Duration::from_secs(event_timeout_secs.max(1)),
            queue: Mutex::new(None),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.zulip")
    }

    fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.server_url, path)
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.basic_auth(&self.bot_email, Some(&self.api_key))
    }

    /// Register a new long-poll event queue. On success, stores the
    /// `queue_id` and initial `last_event_id` in `self.queue`.
    async fn register_queue(&self) -> Result<QueueState> {
        let narrow_json = if self.streams.is_empty() {
            "[]".to_string()
        } else {
            // narrow=[["stream","name"], ...] — Zulip OR's multiple stream
            // narrows so listing all subscribed streams catches everything.
            let parts: Vec<serde_json::Value> = self
                .streams
                .iter()
                .map(|s| serde_json::json!(["stream", s]))
                .collect();
            serde_json::Value::Array(parts).to_string()
        };

        let form = [
            ("event_types", "[\"message\"]"),
            ("narrow", narrow_json.as_str()),
            ("apply_markdown", "false"),
        ];
        let req = self
            .http_client()
            .post(self.rest_url("/api/v1/register"))
            .form(&form);
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .context("Zulip /api/v1/register failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Zulip register returned {status}: {body}");
        }
        let payload: RegisterResponse = resp
            .json()
            .await
            .context("Zulip register returned non-JSON body")?;
        if payload.result != "success" {
            bail!("Zulip register error: {}", payload.msg);
        }
        let queue_id = payload
            .queue_id
            .ok_or_else(|| anyhow::anyhow!("Zulip register response missing queue_id"))?;
        let last_event_id = payload.last_event_id.unwrap_or(-1);
        let state = QueueState {
            queue_id,
            last_event_id,
        };
        *self.queue.lock() = Some(state.clone());
        Ok(state)
    }

    fn advance_queue(&self, latest_event_id: i64) {
        if let Some(ref mut q) = *self.queue.lock()
            && latest_event_id > q.last_event_id
        {
            q.last_event_id = latest_event_id;
        }
    }

    /// Long-poll the events endpoint. Returns `Ok(events)` on success;
    /// `Err` for transport / bad-queue errors so the caller can re-register.
    async fn poll_events(&self, queue: &QueueState) -> Result<Vec<ZulipEvent>> {
        // Use a per-request timeout slightly above the server's hold-open so
        // we don't kill connections that are still legitimately waiting.
        let client = self.http_client();
        let req = client
            .get(self.rest_url("/api/v1/events"))
            .query(&[
                ("queue_id", queue.queue_id.as_str()),
                ("last_event_id", queue.last_event_id.to_string().as_str()),
                ("dont_block", "false"),
            ])
            .timeout(self.event_timeout + Duration::from_secs(10));
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .context("Zulip /api/v1/events request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Zulip events returned {status}: {body}");
        }
        let payload: EventsResponse = resp
            .json()
            .await
            .context("Zulip events returned non-JSON body")?;
        if payload.result != "success" {
            // Code "BAD_EVENT_QUEUE_ID" means the queue expired (inactivity).
            // Surface that as a distinct error so the caller re-registers.
            if payload.code.as_deref() == Some("BAD_EVENT_QUEUE_ID") {
                bail!("BAD_EVENT_QUEUE_ID");
            }
            bail!("Zulip events error: {}", payload.msg);
        }
        Ok(payload.events)
    }

    /// Convert one Zulip `message`-event payload into a `ChannelMessage`,
    /// applying the self-suppression and allowlist filters.
    fn parse_message_event(&self, msg: &ZulipMessage) -> Option<ChannelMessage> {
        if msg.sender_email.eq_ignore_ascii_case(&self.bot_email) {
            return None;
        }
        if msg.last_edit_timestamp.is_some() {
            // Edits are pushed as `update_message` events but appear in the
            // `message` array on some Zulip versions. Treat them as no-ops.
            return None;
        }
        if !is_user_allowed(&msg.sender_email, &self.allowed_users) {
            tracing::debug!(
                "Zulip: dropping message from {} (not in allowed_users)",
                msg.sender_email
            );
            return None;
        }
        let body = msg.content.trim();
        if body.is_empty() {
            return None;
        }

        // Build the reply target so `send` can route back without reparsing.
        let reply_target = match msg.msg_type.as_str() {
            "stream" => match msg.display_recipient.as_str() {
                Some(stream_name) if !msg.subject.trim().is_empty() => {
                    format!("stream:{}/{}", stream_name, msg.subject.trim())
                }
                Some(stream_name) => format!("stream:{stream_name}"),
                None => return None,
            },
            "private" => {
                let emails = extract_private_emails(&msg.display_recipient, &self.bot_email);
                if emails.is_empty() {
                    return None;
                }
                format!("private:{}", emails.join(","))
            }
            other => {
                tracing::debug!("Zulip: skipping unsupported message type {other}");
                return None;
            }
        };

        Some(ChannelMessage {
            id: format!("zulip_{}", msg.id),
            sender: if msg.sender_full_name.is_empty() {
                msg.sender_email.clone()
            } else {
                msg.sender_full_name.clone()
            },
            reply_target,
            content: body.to_string(),
            channel: "zulip".to_string(),
            timestamp: if msg.timestamp >= 0 {
                msg.timestamp as u64
            } else {
                chrono::Utc::now().timestamp().cast_unsigned()
            },
            // `subject` is the topic — surface it for the agent runtime so
            // it can preserve threading without re-parsing reply_target.
            thread_ts: if msg.subject.trim().is_empty() {
                None
            } else {
                Some(msg.subject.clone())
            },
            interruption_scope_id: None,
            attachments: vec![],
        })
    }

    async fn post_chunks(&self, recipient: &Recipient<'_>, chunks: Vec<String>) -> Result<()> {
        let client = self.http_client();
        for chunk in chunks {
            let form = SendForm {
                msg_type: recipient.msg_type,
                to: recipient.to,
                topic: recipient.topic,
                content: &chunk,
            };
            let req = client.post(self.rest_url("/api/v1/messages")).form(&form);
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .context("Zulip /api/v1/messages request failed")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                bail!("Zulip messages POST returned {status}: {body}");
            }
            let payload: SendResponse = resp
                .json()
                .await
                .context("Zulip messages POST returned non-JSON body")?;
            if payload.result != "success" {
                bail!("Zulip send error: {}", payload.msg);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for ZulipChannel {
    fn name(&self) -> &str {
        "zulip"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let parsed = parse_recipient(
            &message.recipient,
            message.thread_ts.as_deref(),
            &self.default_topic,
        )
        .with_context(|| format!("invalid Zulip recipient: {:?}", message.recipient))?;
        let recipient = parsed.borrow();
        let chunks = chunk_text(&message.content, ZULIP_BODY_SOFT_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        self.post_chunks(&recipient, chunks).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!(
            "Zulip channel listening (streams={}, timeout={}s)",
            self.streams.len(),
            self.event_timeout.as_secs()
        );

        let mut backoff = REGISTER_BACKOFF_MIN;
        loop {
            // (Re-)register the queue — also runs at startup.
            let mut queue = match self.register_queue().await {
                Ok(q) => q,
                Err(e) => {
                    tracing::warn!("Zulip register error: {e}; backing off {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(REGISTER_BACKOFF_MAX);
                    continue;
                }
            };
            backoff = REGISTER_BACKOFF_MIN;

            // Inner loop: long-poll until the queue dies or send fails.
            loop {
                let events = match self.poll_events(&queue).await {
                    Ok(e) => e,
                    Err(e) => {
                        let msg = format!("{e:#}");
                        if msg.contains("BAD_EVENT_QUEUE_ID") {
                            tracing::info!("Zulip queue expired, re-registering");
                            *self.queue.lock() = None;
                            break; // outer loop re-registers
                        }
                        tracing::warn!("Zulip poll error: {msg}; pausing");
                        tokio::time::sleep(REGISTER_BACKOFF_MIN).await;
                        continue;
                    }
                };
                if events.is_empty() {
                    continue;
                }

                let mut max_event_id = queue.last_event_id;
                for event in &events {
                    max_event_id = max_event_id.max(event.id);
                    if event.event_type != "message" {
                        // Heartbeats arrive as type="heartbeat"; skip them
                        // along with anything else we haven't subscribed to.
                        let _ = &event.flags; // keep field reachable for future use
                        continue;
                    }
                    let Some(ref msg) = event.message else {
                        continue;
                    };
                    if let Some(channel_msg) = self.parse_message_event(msg)
                        && tx.send(channel_msg).await.is_err()
                    {
                        return Ok(());
                    }
                }
                queue.last_event_id = max_event_id;
                self.advance_queue(max_event_id);
            }
        }
    }

    async fn health_check(&self) -> bool {
        let req = self.http_client().get(self.rest_url("/api/v1/users/me"));
        self.apply_auth(req)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

/// Parsed recipient with the lifetimes lined up for use with reqwest's form().
pub struct OwnedRecipient {
    msg_type: &'static str,
    to: String,
    topic: Option<String>,
}

struct Recipient<'a> {
    msg_type: &'a str,
    to: &'a str,
    topic: Option<&'a str>,
}

impl OwnedRecipient {
    fn borrow(&self) -> Recipient<'_> {
        Recipient {
            msg_type: self.msg_type,
            to: &self.to,
            topic: self.topic.as_deref(),
        }
    }
}

/// Parse a `SendMessage.recipient` string into a structured recipient. See
/// the module docs for the grammar.
pub fn parse_recipient(
    recipient: &str,
    thread_ts: Option<&str>,
    default_topic: &str,
) -> Result<OwnedRecipient> {
    let trimmed = recipient.trim();
    if trimmed.is_empty() {
        bail!("empty recipient");
    }
    if let Some(rest) = trimmed.strip_prefix("stream:") {
        let (stream, topic) = match rest.split_once('/') {
            Some((s, t)) => (s.trim(), Some(t.trim().to_string())),
            None => (rest.trim(), None),
        };
        if stream.is_empty() {
            bail!("stream recipient missing stream name");
        }
        // Zulip accepts a JSON array of stream names for `to` on stream
        // messages — that form sidesteps quoting issues for streams with
        // spaces or commas.
        let to = format!("[\"{}\"]", stream.replace('"', "\\\""));
        let topic = topic
            .filter(|t| !t.is_empty())
            .or_else(|| thread_ts.map(|s| s.to_string()))
            .unwrap_or_else(|| default_topic.to_string());
        Ok(OwnedRecipient {
            msg_type: "stream",
            to,
            topic: Some(topic),
        })
    } else if let Some(rest) = trimmed.strip_prefix("private:") {
        let to = rest.trim().to_string();
        if to.is_empty() {
            bail!("private recipient missing emails");
        }
        Ok(OwnedRecipient {
            msg_type: "private",
            to,
            topic: None,
        })
    } else if trimmed.contains('@') {
        Ok(OwnedRecipient {
            msg_type: "private",
            to: trimmed.to_string(),
            topic: None,
        })
    } else {
        bail!("unrecognized recipient format (expected stream:Name, private:email, or bare email)");
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

/// Allowlist match. `*` matches anyone. Comparison is case-insensitive.
pub fn is_user_allowed(email: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return false;
    }
    if allowlist.iter().any(|u| u == "*") {
        return true;
    }
    let normalized = email.to_ascii_lowercase();
    allowlist
        .iter()
        .any(|entry| entry.to_ascii_lowercase() == normalized)
}

/// Extract the recipient list for a private (DM) message. Drops the bot's own
/// email so a reply doesn't echo back to the bot. Returns the remaining
/// emails sorted to keep multi-party DM addressing stable.
fn extract_private_emails(recipient: &serde_json::Value, bot_email: &str) -> Vec<String> {
    let bot_lc = bot_email.to_ascii_lowercase();
    let arr = match recipient.as_array() {
        Some(a) => a,
        None => return vec![],
    };
    let mut emails: Vec<String> = arr
        .iter()
        .filter_map(|v| {
            v.as_object()
                .and_then(|o| o.get("email"))
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
        .filter(|e| e.to_ascii_lowercase() != bot_lc)
        .collect();
    emails.sort();
    emails.dedup();
    emails
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

    fn channel_with(allowlist: Vec<String>, streams: Vec<String>) -> ZulipChannel {
        ZulipChannel::new(
            "https://zulip.example".into(),
            "agent-bot@example".into(),
            "test-key".into(),
            allowlist,
            streams,
            "agent".into(),
            60,
        )
    }

    fn stream_msg(stream: &str, topic: &str, sender: &str, body: &str) -> ZulipMessage {
        ZulipMessage {
            id: 100,
            msg_type: "stream".into(),
            sender_email: sender.into(),
            sender_full_name: format!("{sender} display"),
            display_recipient: serde_json::Value::String(stream.into()),
            subject: topic.into(),
            content: body.into(),
            timestamp: 1746528000,
            last_edit_timestamp: None,
        }
    }

    fn private_msg(emails: &[&str], sender: &str, body: &str) -> ZulipMessage {
        let arr: Vec<serde_json::Value> = emails
            .iter()
            .map(|e| serde_json::json!({"email": e, "id": 1, "full_name": "u"}))
            .collect();
        ZulipMessage {
            id: 200,
            msg_type: "private".into(),
            sender_email: sender.into(),
            sender_full_name: format!("{sender} display"),
            display_recipient: serde_json::Value::Array(arr),
            subject: "".into(),
            content: body.into(),
            timestamp: 1746528000,
            last_edit_timestamp: None,
        }
    }

    #[test]
    fn normalize_server_url_adds_https_and_strips_trailing_slash() {
        assert_eq!(
            normalize_server_url("zulip.example.com"),
            "https://zulip.example.com"
        );
        assert_eq!(
            normalize_server_url("https://zulip.example.com/"),
            "https://zulip.example.com"
        );
        assert_eq!(
            normalize_server_url("  http://localhost:9991  "),
            "http://localhost:9991"
        );
    }

    #[test]
    fn allowlist_empty_denies_everyone() {
        assert!(!is_user_allowed("alice@example", &[]));
    }

    #[test]
    fn allowlist_wildcard_allows_anyone() {
        let allow = vec!["*".into()];
        assert!(is_user_allowed("alice@example", &allow));
        assert!(is_user_allowed("bob@example", &allow));
    }

    #[test]
    fn allowlist_matches_case_insensitive() {
        let allow = vec!["Alice@Example.COM".into()];
        assert!(is_user_allowed("alice@example.com", &allow));
        assert!(!is_user_allowed("bob@example.com", &allow));
    }

    #[test]
    fn parse_recipient_stream_with_default_topic() {
        let r = parse_recipient("stream:Engineering", None, "agent").unwrap();
        assert_eq!(r.msg_type, "stream");
        assert_eq!(r.to, "[\"Engineering\"]");
        assert_eq!(r.topic.as_deref(), Some("agent"));
    }

    #[test]
    fn parse_recipient_stream_with_topic_in_path() {
        let r = parse_recipient("stream:Engineering/release-prep", None, "agent").unwrap();
        assert_eq!(r.msg_type, "stream");
        assert_eq!(r.topic.as_deref(), Some("release-prep"));
    }

    #[test]
    fn parse_recipient_stream_topic_via_thread_ts() {
        let r = parse_recipient("stream:Engineering", Some("incident-123"), "agent").unwrap();
        assert_eq!(r.topic.as_deref(), Some("incident-123"));
    }

    #[test]
    fn parse_recipient_private_explicit() {
        let r = parse_recipient("private:alice@example,bob@example", None, "agent").unwrap();
        assert_eq!(r.msg_type, "private");
        assert_eq!(r.to, "alice@example,bob@example");
        assert_eq!(r.topic, None);
    }

    #[test]
    fn parse_recipient_private_bare_email() {
        let r = parse_recipient("alice@example", None, "agent").unwrap();
        assert_eq!(r.msg_type, "private");
        assert_eq!(r.to, "alice@example");
    }

    #[test]
    fn parse_recipient_rejects_unknown_format() {
        assert!(parse_recipient("just-a-name", None, "agent").is_err());
        assert!(parse_recipient("", None, "agent").is_err());
        assert!(parse_recipient("stream:", None, "agent").is_err());
    }

    #[test]
    fn parse_message_event_drops_self() {
        let ch = channel_with(vec!["*".into()], vec!["Engineering".into()]);
        let msg = stream_msg("Engineering", "topic", "agent-bot@example", "echo");
        assert!(ch.parse_message_event(&msg).is_none());
    }

    #[test]
    fn parse_message_event_drops_outside_allowlist() {
        let ch = channel_with(vec!["alice@example".into()], vec!["Engineering".into()]);
        let msg = stream_msg("Engineering", "topic", "stranger@example", "hi");
        assert!(ch.parse_message_event(&msg).is_none());
    }

    #[test]
    fn parse_message_event_drops_empty_body() {
        let ch = channel_with(vec!["*".into()], vec![]);
        let msg = stream_msg("Engineering", "topic", "alice@example", "   ");
        assert!(ch.parse_message_event(&msg).is_none());
    }

    #[test]
    fn parse_message_event_drops_edits() {
        let ch = channel_with(vec!["*".into()], vec![]);
        let mut msg = stream_msg("Engineering", "topic", "alice@example", "hi (edited)");
        msg.last_edit_timestamp = Some(1746528100);
        assert!(ch.parse_message_event(&msg).is_none());
    }

    #[test]
    fn parse_message_event_accepts_stream_with_topic() {
        let ch = channel_with(vec!["alice@example".into()], vec!["Engineering".into()]);
        let msg = stream_msg("Engineering", "release-prep", "alice@example", "ping bot");
        let ch_msg = ch.parse_message_event(&msg).expect("expected msg");
        assert_eq!(ch_msg.channel, "zulip");
        assert_eq!(ch_msg.id, "zulip_100");
        assert_eq!(ch_msg.reply_target, "stream:Engineering/release-prep");
        assert_eq!(ch_msg.thread_ts.as_deref(), Some("release-prep"));
        assert_eq!(ch_msg.content, "ping bot");
        assert!(ch_msg.sender.contains("alice"));
    }

    #[test]
    fn parse_message_event_accepts_stream_without_topic() {
        let ch = channel_with(vec!["alice@example".into()], vec!["Engineering".into()]);
        let msg = stream_msg("Engineering", "  ", "alice@example", "no topic");
        let ch_msg = ch.parse_message_event(&msg).expect("expected msg");
        assert_eq!(ch_msg.reply_target, "stream:Engineering");
        assert!(ch_msg.thread_ts.is_none());
    }

    #[test]
    fn parse_message_event_accepts_private_excluding_self() {
        let ch = channel_with(vec!["alice@example".into()], vec![]);
        let msg = private_msg(
            &["alice@example", "agent-bot@example"],
            "alice@example",
            "ping",
        );
        let ch_msg = ch.parse_message_event(&msg).expect("expected msg");
        assert_eq!(ch_msg.reply_target, "private:alice@example");
    }

    #[test]
    fn parse_message_event_drops_unknown_message_type() {
        let ch = channel_with(vec!["*".into()], vec![]);
        let mut msg = stream_msg("Engineering", "topic", "alice@example", "hi");
        msg.msg_type = "huddle".into();
        assert!(ch.parse_message_event(&msg).is_none());
    }

    #[test]
    fn chunk_text_short_passes_through() {
        let chunks = chunk_text("hi there", 8000);
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
        let chunks = chunk_text("   ", 8000);
        assert!(chunks.is_empty());
    }

    #[test]
    fn extract_private_emails_drops_bot_self_and_dedups() {
        let recipient = serde_json::json!([
            {"email": "alice@example", "id": 1, "full_name": "A"},
            {"email": "agent-bot@example", "id": 2, "full_name": "Bot"},
            {"email": "bob@example", "id": 3, "full_name": "B"},
            {"email": "alice@example", "id": 1, "full_name": "A"},
        ]);
        let emails = extract_private_emails(&recipient, "agent-bot@example");
        assert_eq!(emails, vec!["alice@example", "bob@example"]);
    }

    mod http_tests {
        use super::*;
        use serde_json::json;
        use wiremock::matchers::{
            body_string_contains, header, header_exists, method, path, query_param,
        };
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn channel_for(server_uri: &str) -> ZulipChannel {
            ZulipChannel::new(
                server_uri.to_string(),
                "agent-bot@example".into(),
                "test-key".into(),
                vec!["*".into()],
                vec!["Engineering".into()],
                "agent".into(),
                60,
            )
        }

        #[tokio::test]
        async fn send_stream_posts_form_with_basic_auth() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/messages"))
                .and(header_exists("authorization"))
                .and(header("content-type", "application/x-www-form-urlencoded"))
                .and(body_string_contains("type=stream"))
                .and(body_string_contains("topic=release-prep"))
                .and(body_string_contains("content=hello"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "result": "success",
                    "msg": "",
                    "id": 1
                })))
                .expect(1)
                .mount(&server)
                .await;

            channel_for(&server.uri())
                .send(&SendMessage {
                    content: "hello".into(),
                    recipient: "stream:Engineering/release-prep".into(),
                    subject: None,
                    thread_ts: None,
                    cancellation_token: None,
                    attachments: vec![],
                })
                .await
                .expect("send succeeded");
        }

        #[tokio::test]
        async fn send_private_posts_form_to_emails() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/messages"))
                .and(body_string_contains("type=private"))
                .and(body_string_contains("alice"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "result": "success",
                    "msg": ""
                })))
                .expect(1)
                .mount(&server)
                .await;

            channel_for(&server.uri())
                .send(&SendMessage {
                    content: "ping".into(),
                    recipient: "private:alice@example".into(),
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
                .and(path("/api/v1/messages"))
                .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
                .mount(&server)
                .await;

            let err = channel_for(&server.uri())
                .send(&SendMessage {
                    content: "x".into(),
                    recipient: "stream:Engineering".into(),
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
        async fn send_surfaces_result_error_payload() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/messages"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "result": "error",
                    "msg": "Stream does not exist"
                })))
                .mount(&server)
                .await;

            let err = channel_for(&server.uri())
                .send(&SendMessage {
                    content: "x".into(),
                    recipient: "stream:Nope".into(),
                    subject: None,
                    thread_ts: None,
                    cancellation_token: None,
                    attachments: vec![],
                })
                .await
                .expect_err("expected error");
            assert!(format!("{err:#}").contains("Stream does not exist"));
        }

        #[tokio::test]
        async fn register_queue_returns_id_and_initial_event() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/register"))
                .and(header_exists("authorization"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "result": "success",
                    "msg": "",
                    "queue_id": "Q1",
                    "last_event_id": 42
                })))
                .expect(1)
                .mount(&server)
                .await;

            let q = channel_for(&server.uri())
                .register_queue()
                .await
                .expect("register ok");
            assert_eq!(q.queue_id, "Q1");
            assert_eq!(q.last_event_id, 42);
        }

        #[tokio::test]
        async fn poll_events_returns_events_and_signals_bad_queue() {
            let server = MockServer::start().await;
            // Success path
            Mock::given(method("GET"))
                .and(path("/api/v1/events"))
                .and(query_param("queue_id", "Q1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "result": "success",
                    "msg": "",
                    "events": [{
                        "id": 100,
                        "type": "message",
                        "message": {
                            "id": 9000,
                            "type": "stream",
                            "sender_email": "alice@example",
                            "sender_full_name": "Alice",
                            "display_recipient": "Engineering",
                            "subject": "topic",
                            "content": "hi bot",
                            "timestamp": 1746528000
                        }
                    }]
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            let events = ch
                .poll_events(&QueueState {
                    queue_id: "Q1".into(),
                    last_event_id: -1,
                })
                .await
                .expect("poll ok");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].id, 100);

            // Bad queue path
            let server_bad = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/events"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "result": "error",
                    "msg": "Bad event queue id",
                    "code": "BAD_EVENT_QUEUE_ID"
                })))
                .mount(&server_bad)
                .await;
            let err = channel_for(&server_bad.uri())
                .poll_events(&QueueState {
                    queue_id: "Q1".into(),
                    last_event_id: -1,
                })
                .await
                .expect_err("expected bad-queue err");
            assert!(format!("{err:#}").contains("BAD_EVENT_QUEUE_ID"));
        }

        #[tokio::test]
        async fn health_check_uses_users_me() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/users/me"))
                .and(header_exists("authorization"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "result": "success",
                    "email": "agent-bot@example"
                })))
                .expect(1)
                .mount(&server)
                .await;
            assert!(channel_for(&server.uri()).health_check().await);
        }
    }
}
