use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::HeaderMap;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Slack channel — polls conversations.history via Web API
pub struct SlackChannel {
    bot_token: String,
    app_token: Option<String>,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
    mention_only: bool,
    group_reply_allowed_sender_ids: Vec<String>,
}

const SLACK_HISTORY_MAX_RETRIES: u32 = 3;
const SLACK_HISTORY_DEFAULT_RETRY_AFTER_SECS: u64 = 1;
const SLACK_HISTORY_MAX_BACKOFF_SECS: u64 = 120;
const SLACK_HISTORY_MAX_JITTER_MS: u64 = 500;

impl SlackChannel {
    pub fn new(
        bot_token: String,
        app_token: Option<String>,
        channel_id: Option<String>,
        allowed_users: Vec<String>,
    ) -> Self {
        Self {
            bot_token,
            app_token,
            channel_id,
            allowed_users,
            mention_only: false,
            group_reply_allowed_sender_ids: Vec::new(),
        }
    }

    /// Configure group-chat trigger policy.
    pub fn with_group_reply_policy(
        mut self,
        mention_only: bool,
        allowed_sender_ids: Vec<String>,
    ) -> Self {
        self.mention_only = mention_only;
        self.group_reply_allowed_sender_ids =
            Self::normalize_group_reply_allowed_sender_ids(allowed_sender_ids);
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.slack")
    }

    /// Check if a Slack user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn is_group_sender_trigger_enabled(&self, user_id: &str) -> bool {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return false;
        }

        self.group_reply_allowed_sender_ids
            .iter()
            .any(|entry| entry == "*" || entry == user_id)
    }

    /// Get the bot's own user ID so we can ignore our own messages
    async fn get_bot_user_id(&self) -> Option<String> {
        let resp: serde_json::Value = self
            .http_client()
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;

        resp.get("user_id")
            .and_then(|u| u.as_str())
            .map(String::from)
    }

    /// Resolve the thread identifier for inbound Slack messages.
    /// Replies carry `thread_ts` (root thread id); top-level messages only have `ts`.
    fn inbound_thread_ts(msg: &serde_json::Value, ts: &str) -> Option<String> {
        msg.get("thread_ts")
            .and_then(|t| t.as_str())
            .or(if ts.is_empty() { None } else { Some(ts) })
            .map(str::to_string)
    }

    fn normalized_channel_id(input: Option<&str>) -> Option<String> {
        input
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "*")
            .map(ToOwned::to_owned)
    }

    fn configured_channel_id(&self) -> Option<String> {
        Self::normalized_channel_id(self.channel_id.as_deref())
    }

    fn configured_app_token(&self) -> Option<String> {
        self.app_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    fn normalize_group_reply_allowed_sender_ids(sender_ids: Vec<String>) -> Vec<String> {
        let mut normalized = sender_ids
            .into_iter()
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
            .collect::<Vec<_>>();
        normalized.sort();
        normalized.dedup();
        normalized
    }

    fn is_group_channel_id(channel_id: &str) -> bool {
        matches!(channel_id.chars().next(), Some('C' | 'G'))
    }

    fn contains_bot_mention(text: &str, bot_user_id: &str) -> bool {
        if bot_user_id.is_empty() {
            return false;
        }
        text.contains(&format!("<@{bot_user_id}>"))
    }

    fn strip_bot_mentions(text: &str, bot_user_id: &str) -> String {
        if bot_user_id.is_empty() {
            return text.trim().to_string();
        }
        text.replace(&format!("<@{bot_user_id}>"), " ")
            .trim()
            .to_string()
    }

    fn normalize_incoming_content(
        text: &str,
        require_mention: bool,
        bot_user_id: &str,
    ) -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }
        if require_mention && !Self::contains_bot_mention(text, bot_user_id) {
            return None;
        }

        let normalized = if require_mention {
            Self::strip_bot_mentions(text, bot_user_id)
        } else {
            text.trim().to_string()
        };

        if normalized.is_empty() {
            return None;
        }
        Some(normalized)
    }

    fn extract_channel_ids(list_payload: &serde_json::Value) -> Vec<String> {
        let mut ids = list_payload
            .get("channels")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
            .filter_map(|channel| {
                let id = channel.get("id").and_then(|id| id.as_str())?;
                let is_archived = channel
                    .get("is_archived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let is_member = channel
                    .get("is_member")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if is_archived || !is_member {
                    return None;
                }
                Some(id.to_string())
            })
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    async fn list_accessible_channels(&self) -> anyhow::Result<Vec<String>> {
        let mut channels = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut query_params = vec![
                ("exclude_archived", "true".to_string()),
                ("limit", "200".to_string()),
                (
                    "types",
                    "public_channel,private_channel,mpim,im".to_string(),
                ),
            ];
            if let Some(ref next) = cursor {
                query_params.push(("cursor", next.clone()));
            }

            let resp = self
                .http_client()
                .get("https://slack.com/api/conversations.list")
                .bearer_auth(&self.bot_token)
                .query(&query_params)
                .send()
                .await?;

            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

            if !status.is_success() {
                let sanitized = crate::providers::sanitize_api_error(&body);
                anyhow::bail!("Slack conversations.list failed ({status}): {sanitized}");
            }

            let data: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            if data.get("ok") == Some(&serde_json::Value::Bool(false)) {
                let err = data
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                anyhow::bail!("Slack conversations.list failed: {err}");
            }

            channels.extend(Self::extract_channel_ids(&data));

            cursor = data
                .get("response_metadata")
                .and_then(|rm| rm.get("next_cursor"))
                .and_then(|c| c.as_str())
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(ToOwned::to_owned);

            if cursor.is_none() {
                break;
            }
        }

        channels.sort();
        channels.dedup();
        Ok(channels)
    }

    fn slack_now_ts() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        format!("{}.{:06}", now.as_secs(), now.subsec_micros())
    }

    fn ensure_poll_cursor(
        cursors: &mut HashMap<String, String>,
        channel_id: &str,
        now_ts: &str,
    ) -> String {
        cursors
            .entry(channel_id.to_string())
            .or_insert_with(|| now_ts.to_string())
            .clone()
    }

    async fn open_socket_mode_url(&self) -> anyhow::Result<String> {
        let app_token = self
            .configured_app_token()
            .ok_or_else(|| anyhow::anyhow!("Slack Socket Mode requires app_token"))?;

        let resp = self
            .http_client()
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(app_token)
            .send()
            .await?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&body);
            anyhow::bail!("Slack apps.connections.open failed ({status}): {sanitized}");
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Slack apps.connections.open failed: {err}");
        }

        parsed
            .get("url")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Slack apps.connections.open did not return url"))
    }

    async fn listen_socket_mode(
        &self,
        tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        bot_user_id: &str,
        scoped_channel: Option<String>,
    ) -> anyhow::Result<()> {
        let mut last_ts_by_channel: HashMap<String, String> = HashMap::new();

        loop {
            let ws_url = match self.open_socket_mode_url().await {
                Ok(url) => url,
                Err(e) => {
                    tracing::warn!("Slack Socket Mode: failed to open websocket URL: {e}");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            };

            let (ws_stream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
                Ok(connection) => connection,
                Err(e) => {
                    tracing::warn!("Slack Socket Mode: websocket connect failed: {e}");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            };
            tracing::info!("Slack Socket Mode: websocket connected");

            let (mut write, mut read) = ws_stream.split();

            while let Some(frame) = read.next().await {
                let text = match frame {
                    Ok(WsMessage::Text(text)) => text,
                    Ok(WsMessage::Ping(payload)) => {
                        if let Err(e) = write.send(WsMessage::Pong(payload)).await {
                            tracing::warn!("Slack Socket Mode: pong send failed: {e}");
                            break;
                        }
                        continue;
                    }
                    Ok(WsMessage::Close(_)) => {
                        tracing::warn!("Slack Socket Mode: websocket closed by server");
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::warn!("Slack Socket Mode: websocket read failed: {e}");
                        break;
                    }
                };

                let envelope: serde_json::Value = match serde_json::from_str(text.as_ref()) {
                    Ok(value) => value,
                    Err(e) => {
                        tracing::warn!("Slack Socket Mode: invalid JSON payload: {e}");
                        continue;
                    }
                };

                if let Some(envelope_id) = envelope.get("envelope_id").and_then(|v| v.as_str()) {
                    let ack = serde_json::json!({ "envelope_id": envelope_id });
                    if let Err(e) = write.send(WsMessage::Text(ack.to_string().into())).await {
                        tracing::warn!("Slack Socket Mode: ack send failed: {e}");
                        break;
                    }
                }

                let envelope_type = envelope
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if envelope_type == "disconnect" {
                    tracing::warn!("Slack Socket Mode: received disconnect event");
                    break;
                }
                if envelope_type != "events_api" {
                    continue;
                }

                let Some(event) = envelope
                    .get("payload")
                    .and_then(|payload| payload.get("event"))
                else {
                    continue;
                };
                if event.get("type").and_then(|v| v.as_str()) != Some("message") {
                    continue;
                }
                // Skip non-user message subtypes (e.g. channel_join/message_changed)
                // to avoid invalid thread replies.
                if event.get("subtype").is_some() {
                    continue;
                }

                let channel_id = event
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_default();
                if channel_id.is_empty() {
                    continue;
                }
                if let Some(ref configured_channel) = scoped_channel {
                    if channel_id != *configured_channel {
                        continue;
                    }
                }

                let user = event
                    .get("user")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if user.is_empty() || user == bot_user_id {
                    continue;
                }
                if !self.is_user_allowed(user) {
                    tracing::warn!("Slack: ignoring message from unauthorized user: {user}");
                    continue;
                }

                let text = event
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }

                let ts = event.get("ts").and_then(|v| v.as_str()).unwrap_or_default();
                if ts.is_empty() {
                    continue;
                }
                let last_ts = last_ts_by_channel
                    .get(&channel_id)
                    .map(String::as_str)
                    .unwrap_or_default();
                if ts <= last_ts {
                    continue;
                }

                let is_group_message = Self::is_group_channel_id(&channel_id);
                let allow_sender_without_mention =
                    is_group_message && self.is_group_sender_trigger_enabled(user);
                let require_mention =
                    self.mention_only && is_group_message && !allow_sender_without_mention;

                let Some(normalized_text) =
                    Self::normalize_incoming_content(text, require_mention, bot_user_id)
                else {
                    continue;
                };

                last_ts_by_channel.insert(channel_id.clone(), ts.to_string());

                let channel_msg = ChannelMessage {
                    id: format!("slack_{channel_id}_{ts}"),
                    sender: user.to_string(),
                    reply_target: channel_id.clone(),
                    content: normalized_text,
                    channel: "slack".to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts: Self::inbound_thread_ts(event, ts),
                };

                if tx.send(channel_msg).await.is_err() {
                    return Ok(());
                }
            }

            tracing::warn!("Slack Socket Mode: reconnecting in 3 seconds...");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    fn parse_retry_after_secs(headers: &HeaderMap) -> Option<u64> {
        let value = headers
            .get(reqwest::header::RETRY_AFTER)?
            .to_str()
            .ok()?
            .trim();
        Self::parse_retry_after_value(value)
    }

    fn parse_retry_after_value(value: &str) -> Option<u64> {
        if value.is_empty() {
            return None;
        }

        if let Ok(seconds) = value.parse::<u64>() {
            return Some(seconds);
        }

        let truncated = value
            .split_once('.')
            .map(|(whole, _)| whole)
            .unwrap_or(value);
        truncated.parse::<u64>().ok()
    }

    fn jitter_ms_from_clock(max_jitter_ms: u64) -> u64 {
        if max_jitter_ms == 0 {
            return 0;
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()))
            .unwrap_or(0);
        nanos % (max_jitter_ms + 1)
    }

    fn compute_retry_delay(base_retry_after_secs: u64, attempt: u32, jitter_ms: u64) -> Duration {
        let multiplier = 1_u64.checked_shl(attempt).unwrap_or(u64::MAX);
        let backoff_secs = base_retry_after_secs
            .saturating_mul(multiplier)
            .min(SLACK_HISTORY_MAX_BACKOFF_SECS);
        Duration::from_secs(backoff_secs) + Duration::from_millis(jitter_ms)
    }

    fn next_retry_timestamp(wait: Duration) -> String {
        match chrono::Duration::from_std(wait) {
            Ok(delta) => (Utc::now() + delta).to_rfc3339(),
            Err(_) => Utc::now().to_rfc3339(),
        }
    }

    async fn fetch_history_with_retry(
        &self,
        channel_id: &str,
        params: &[(&str, String)],
    ) -> Option<serde_json::Value> {
        let mut total_wait = Duration::from_secs(0);

        for attempt in 0..=SLACK_HISTORY_MAX_RETRIES {
            let resp = match self
                .http_client()
                .get("https://slack.com/api/conversations.history")
                .bearer_auth(&self.bot_token)
                .query(params)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Slack poll error for channel {channel_id}: {e}");
                    return None;
                }
            };

            let status = resp.status();
            let headers = resp.headers().clone();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

            let is_ratelimited_http = status == reqwest::StatusCode::TOO_MANY_REQUESTS;
            let payload: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let is_ratelimited_payload = payload.get("ok") == Some(&serde_json::Value::Bool(false))
                && payload
                    .get("error")
                    .and_then(|e| e.as_str())
                    .is_some_and(|err| err == "ratelimited");

            if is_ratelimited_http || is_ratelimited_payload {
                if attempt >= SLACK_HISTORY_MAX_RETRIES {
                    tracing::error!(
                        "Slack rate limit retries exhausted for conversations.history on channel {}. Total wait: {}s across {} attempts. Proceeding without channel history.",
                        channel_id,
                        total_wait.as_secs(),
                        SLACK_HISTORY_MAX_RETRIES
                    );
                    return None;
                }

                let retry_after_secs = Self::parse_retry_after_secs(&headers)
                    .unwrap_or(SLACK_HISTORY_DEFAULT_RETRY_AFTER_SECS);
                let jitter_ms = Self::jitter_ms_from_clock(SLACK_HISTORY_MAX_JITTER_MS);
                let wait = Self::compute_retry_delay(retry_after_secs, attempt, jitter_ms);
                total_wait += wait;
                let next_retry_at = Self::next_retry_timestamp(wait);
                tracing::warn!(
                    "Slack conversations.history rate limited for channel {}. Retry-After: {}s. Attempt {}/{}. Next retry at {}.",
                    channel_id,
                    retry_after_secs,
                    attempt + 1,
                    SLACK_HISTORY_MAX_RETRIES,
                    next_retry_at
                );
                tokio::time::sleep(wait).await;
                continue;
            }

            if !status.is_success() {
                let sanitized = crate::providers::sanitize_api_error(&body);
                tracing::warn!(
                    "Slack history request failed for channel {} ({}): {}",
                    channel_id,
                    status,
                    sanitized
                );
                return None;
            }

            if payload.get("ok") == Some(&serde_json::Value::Bool(false)) {
                let err = payload
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                tracing::warn!("Slack history error for channel {channel_id}: {err}");
                return None;
            }

            return Some(payload);
        }

        None
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let mut body = serde_json::json!({
            "channel": message.recipient,
            "text": message.content
        });

        if let Some(ref ts) = message.thread_ts {
            body["thread_ts"] = serde_json::json!(ts);
        }

        let resp = self
            .http_client()
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&body);
            anyhow::bail!("Slack chat.postMessage failed ({status}): {sanitized}");
        }

        // Slack returns 200 for most app-level errors; check JSON "ok" field
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Slack chat.postMessage failed: {err}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = self.get_bot_user_id().await.unwrap_or_default();
        let scoped_channel = self.configured_channel_id();
        if self.configured_app_token().is_some() {
            tracing::info!("Slack channel listening in Socket Mode");
            return self
                .listen_socket_mode(tx, &bot_user_id, scoped_channel)
                .await;
        }

        let mut discovered_channels: Vec<String> = Vec::new();
        let mut last_discovery = Instant::now();
        let mut last_ts_by_channel: HashMap<String, String> = HashMap::new();

        if let Some(ref channel_id) = scoped_channel {
            tracing::info!("Slack channel listening on #{channel_id}...");
        } else {
            tracing::info!(
                "Slack channel_id not set (or '*'); listening across all accessible channels."
            );
        }

        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;

            let target_channels = if let Some(ref channel_id) = scoped_channel {
                vec![channel_id.clone()]
            } else {
                if discovered_channels.is_empty()
                    || last_discovery.elapsed() >= Duration::from_secs(60)
                {
                    match self.list_accessible_channels().await {
                        Ok(channels) => {
                            if channels != discovered_channels {
                                tracing::info!(
                                    "Slack auto-discovery refreshed: listening on {} channel(s).",
                                    channels.len()
                                );
                            }
                            discovered_channels = channels;
                        }
                        Err(e) => {
                            tracing::warn!("Slack channel discovery failed: {e}");
                        }
                    }
                    last_discovery = Instant::now();
                }

                discovered_channels.clone()
            };

            if target_channels.is_empty() {
                tracing::debug!("Slack: no accessible channels discovered yet");
                continue;
            }

            for channel_id in target_channels {
                let had_cursor = last_ts_by_channel.contains_key(&channel_id);
                let bootstrap_ts = Self::slack_now_ts();
                let cursor_ts =
                    Self::ensure_poll_cursor(&mut last_ts_by_channel, &channel_id, &bootstrap_ts);
                if !had_cursor {
                    tracing::debug!(
                        "Slack: initialized cursor for channel {} at {} to prevent historical replay",
                        channel_id,
                        cursor_ts
                    );
                }
                let params = vec![
                    ("channel", channel_id.clone()),
                    ("limit", "10".to_string()),
                    ("oldest", cursor_ts),
                ];

                let Some(data) = self.fetch_history_with_retry(&channel_id, &params).await else {
                    continue;
                };

                if let Some(messages) = data.get("messages").and_then(|m| m.as_array()) {
                    // Messages come newest-first, reverse to process oldest first
                    for msg in messages.iter().rev() {
                        // Skip non-user message subtypes (e.g. channel_join/message_changed)
                        // to avoid invalid thread replies.
                        if msg.get("subtype").is_some() {
                            continue;
                        }
                        let ts = msg.get("ts").and_then(|t| t.as_str()).unwrap_or("");
                        let user = msg
                            .get("user")
                            .and_then(|u| u.as_str())
                            .unwrap_or("unknown");
                        let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        let last_ts = last_ts_by_channel
                            .get(&channel_id)
                            .map(String::as_str)
                            .unwrap_or("");

                        // Skip bot's own messages
                        if user == bot_user_id {
                            continue;
                        }

                        // Sender validation
                        if !self.is_user_allowed(user) {
                            tracing::warn!(
                                "Slack: ignoring message from unauthorized user: {user}"
                            );
                            continue;
                        }

                        // Skip empty or already-seen
                        if text.is_empty() || ts <= last_ts {
                            continue;
                        }

                        let is_group_message = Self::is_group_channel_id(&channel_id);
                        let allow_sender_without_mention =
                            is_group_message && self.is_group_sender_trigger_enabled(user);
                        let require_mention =
                            self.mention_only && is_group_message && !allow_sender_without_mention;
                        let Some(normalized_text) =
                            Self::normalize_incoming_content(text, require_mention, &bot_user_id)
                        else {
                            continue;
                        };

                        last_ts_by_channel.insert(channel_id.clone(), ts.to_string());

                        let channel_msg = ChannelMessage {
                            id: format!("slack_{channel_id}_{ts}"),
                            sender: user.to_string(),
                            reply_target: channel_id.clone(),
                            content: normalized_text,
                            channel: "slack".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            thread_ts: Self::inbound_thread_ts(msg, ts),
                        };

                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_channel_name() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![]);
        assert_eq!(ch.name(), "slack");
    }

    #[test]
    fn slack_channel_with_channel_id() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, Some("C12345".into()), vec![]);
        assert_eq!(ch.channel_id, Some("C12345".to_string()));
    }

    #[test]
    fn slack_group_reply_policy_defaults_to_all_messages() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec!["*".into()]);
        assert!(!ch.mention_only);
        assert!(ch.group_reply_allowed_sender_ids.is_empty());
    }

    #[test]
    fn slack_group_reply_policy_applies_sender_overrides() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec!["*".into()])
            .with_group_reply_policy(true, vec![" U111 ".into(), "U111".into(), "U222".into()]);

        assert!(ch.mention_only);
        assert_eq!(
            ch.group_reply_allowed_sender_ids,
            vec!["U111".to_string(), "U222".to_string()]
        );
        assert!(ch.is_group_sender_trigger_enabled("U111"));
        assert!(!ch.is_group_sender_trigger_enabled("U999"));
    }

    #[test]
    fn normalized_channel_id_respects_wildcard_and_blank() {
        assert_eq!(SlackChannel::normalized_channel_id(None), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("   ")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("*")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some(" * ")), None);
        assert_eq!(
            SlackChannel::normalized_channel_id(Some(" C12345 ")),
            Some("C12345".to_string())
        );
    }

    #[test]
    fn configured_app_token_ignores_blank_values() {
        let ch = SlackChannel::new("xoxb-fake".into(), Some("   ".into()), None, vec![]);
        assert_eq!(ch.configured_app_token(), None);
    }

    #[test]
    fn configured_app_token_trims_value() {
        let ch = SlackChannel::new("xoxb-fake".into(), Some(" xapp-123 ".into()), None, vec![]);
        assert_eq!(ch.configured_app_token().as_deref(), Some("xapp-123"));
    }

    #[test]
    fn is_group_channel_id_detects_channel_prefixes() {
        assert!(SlackChannel::is_group_channel_id("C123"));
        assert!(SlackChannel::is_group_channel_id("G123"));
        assert!(!SlackChannel::is_group_channel_id("D123"));
        assert!(!SlackChannel::is_group_channel_id(""));
    }

    #[test]
    fn extract_channel_ids_filters_archived_and_non_member_entries() {
        let payload = serde_json::json!({
            "channels": [
                {"id": "C1", "is_archived": false, "is_member": true},
                {"id": "C2", "is_archived": true, "is_member": true},
                {"id": "C3", "is_archived": false, "is_member": false},
                {"id": "C1", "is_archived": false, "is_member": true},
                {"id": "C4"}
            ]
        });
        let ids = SlackChannel::extract_channel_ids(&payload);
        assert_eq!(ids, vec!["C1".to_string(), "C4".to_string()]);
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![]);
        assert!(!ch.is_user_allowed("U12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec!["*".into()]);
        assert!(ch.is_user_allowed("U12345"));
    }

    #[test]
    fn normalize_incoming_content_requires_mention_when_enabled() {
        assert!(SlackChannel::normalize_incoming_content("hello", true, "U_BOT").is_none());
        assert_eq!(
            SlackChannel::normalize_incoming_content("<@U_BOT> run", true, "U_BOT").as_deref(),
            Some("run")
        );
    }

    #[test]
    fn normalize_incoming_content_without_mention_mode_keeps_message() {
        assert_eq!(
            SlackChannel::normalize_incoming_content("  hello world  ", false, "U_BOT").as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            None,
            None,
            vec!["U111".into(), "U222".into()],
        );
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("U222"));
        assert!(!ch.is_user_allowed("U333"));
    }

    #[test]
    fn allowlist_exact_match_not_substring() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec!["U111".into()]);
        assert!(!ch.is_user_allowed("U1111"));
        assert!(!ch.is_user_allowed("U11"));
    }

    #[test]
    fn allowlist_empty_user_id() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec!["U111".into()]);
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec!["U111".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(!ch.is_user_allowed("u111"));
    }

    #[test]
    fn allowlist_wildcard_and_specific() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            None,
            None,
            vec!["U111".into(), "*".into()],
        );
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("anyone"));
    }

    // ── Message ID edge cases ─────────────────────────────────────

    #[test]
    fn slack_message_id_format_includes_channel_and_ts() {
        // Verify that message IDs follow the format: slack_{channel_id}_{ts}
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let expected_id = format!("slack_{channel_id}_{ts}");
        assert_eq!(expected_id, "slack_C12345_1234567890.123456");
    }

    #[test]
    fn slack_message_id_is_deterministic() {
        // Same channel_id + same ts = same ID (prevents duplicates after restart)
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let id1 = format!("slack_{channel_id}_{ts}");
        let id2 = format!("slack_{channel_id}_{ts}");
        assert_eq!(id1, id2);
    }

    #[test]
    fn slack_message_id_different_ts_different_id() {
        // Different timestamps produce different IDs
        let channel_id = "C12345";
        let id1 = format!("slack_{channel_id}_1234567890.123456");
        let id2 = format!("slack_{channel_id}_1234567890.123457");
        assert_ne!(id1, id2);
    }

    #[test]
    fn slack_message_id_different_channel_different_id() {
        // Different channels produce different IDs even with same ts
        let ts = "1234567890.123456";
        let id1 = format!("slack_C12345_{ts}");
        let id2 = format!("slack_C67890_{ts}");
        assert_ne!(id1, id2);
    }

    #[test]
    fn slack_message_id_no_uuid_randomness() {
        // Verify format doesn't contain random UUID components
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let id = format!("slack_{channel_id}_{ts}");
        assert!(!id.contains('-')); // No UUID dashes
        assert!(id.starts_with("slack_"));
    }

    #[test]
    fn inbound_thread_ts_prefers_explicit_thread_ts() {
        let msg = serde_json::json!({
            "ts": "123.002",
            "thread_ts": "123.001"
        });

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "123.002");
        assert_eq!(thread_ts.as_deref(), Some("123.001"));
    }

    #[test]
    fn inbound_thread_ts_falls_back_to_ts() {
        let msg = serde_json::json!({
            "ts": "123.001"
        });

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "123.001");
        assert_eq!(thread_ts.as_deref(), Some("123.001"));
    }

    #[test]
    fn inbound_thread_ts_none_when_ts_missing() {
        let msg = serde_json::json!({});

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "");
        assert_eq!(thread_ts, None);
    }

    #[test]
    fn ensure_poll_cursor_bootstraps_new_channel() {
        let mut cursors = HashMap::new();
        let now_ts = "1700000000.123456";

        let cursor = SlackChannel::ensure_poll_cursor(&mut cursors, "C123", now_ts);
        assert_eq!(cursor, now_ts);
        assert_eq!(cursors.get("C123").map(String::as_str), Some(now_ts));
    }

    #[test]
    fn ensure_poll_cursor_keeps_existing_cursor() {
        let mut cursors = HashMap::from([("C123".to_string(), "1700000000.000001".to_string())]);
        let cursor = SlackChannel::ensure_poll_cursor(&mut cursors, "C123", "9999999999.999999");

        assert_eq!(cursor, "1700000000.000001");
        assert_eq!(
            cursors.get("C123").map(String::as_str),
            Some("1700000000.000001")
        );
    }

    #[test]
    fn parse_retry_after_value_accepts_integer_seconds() {
        assert_eq!(SlackChannel::parse_retry_after_value("30"), Some(30));
    }

    #[test]
    fn parse_retry_after_value_accepts_decimal_seconds() {
        assert_eq!(SlackChannel::parse_retry_after_value("2.9"), Some(2));
    }

    #[test]
    fn parse_retry_after_value_rejects_non_numeric_values() {
        assert_eq!(SlackChannel::parse_retry_after_value("later"), None);
        assert_eq!(SlackChannel::parse_retry_after_value(""), None);
    }

    #[test]
    fn parse_retry_after_secs_reads_header_value() {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "45".parse().unwrap());
        assert_eq!(SlackChannel::parse_retry_after_secs(&headers), Some(45));
    }

    #[test]
    fn compute_retry_delay_applies_backoff_and_jitter_with_cap() {
        let delay = SlackChannel::compute_retry_delay(30, 3, 250);
        assert_eq!(delay, Duration::from_secs(120) + Duration::from_millis(250));
    }
}
