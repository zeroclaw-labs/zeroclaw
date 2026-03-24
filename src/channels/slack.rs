use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Context;
use async_trait::async_trait;
use base64::Engine as _;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::HeaderMap;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Clone)]
struct CachedSlackDisplayName {
    display_name: String,
    expires_at: Instant,
}

/// Slack channel — polls conversations.history via Web API
pub struct SlackChannel {
    bot_token: String,
    app_token: Option<String>,
    channel_id: Option<String>,
    channel_ids: Vec<String>,
    allowed_users: Vec<String>,
    thread_replies: bool,
    mention_only: bool,
    group_reply_allowed_sender_ids: Vec<String>,
    user_display_name_cache: Mutex<HashMap<String, CachedSlackDisplayName>>,
    workspace_dir: Option<PathBuf>,
    /// Maps channel_id -> thread_ts for active assistant threads (used for status indicators).
    active_assistant_thread: Mutex<HashMap<String, String>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Voice transcription config — when set, audio file attachments are
    /// downloaded, transcribed, and their text inlined into the message.
    transcription: Option<crate::config::TranscriptionConfig>,
}

const SLACK_HISTORY_MAX_RETRIES: u32 = 3;
const SLACK_HISTORY_DEFAULT_RETRY_AFTER_SECS: u64 = 1;
const SLACK_HISTORY_MAX_BACKOFF_SECS: u64 = 120;
const SLACK_HISTORY_MAX_JITTER_MS: u64 = 500;
const SLACK_SOCKET_MODE_INITIAL_BACKOFF_SECS: u64 = 3;
const SLACK_SOCKET_MODE_MAX_BACKOFF_SECS: u64 = 120;
const SLACK_SOCKET_MODE_MAX_JITTER_MS: u64 = 500;
const SLACK_USER_CACHE_TTL_SECS: u64 = 6 * 60 * 60;
const SLACK_ATTACHMENT_IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;
const SLACK_ATTACHMENT_IMAGE_INLINE_FALLBACK_MAX_BYTES: usize = 512 * 1024;
const SLACK_ATTACHMENT_TEXT_DOWNLOAD_MAX_BYTES: usize = 256 * 1024;
const SLACK_ATTACHMENT_TEXT_INLINE_MAX_CHARS: usize = 12_000;
const SLACK_MARKDOWN_BLOCK_MAX_CHARS: usize = 12_000;
const SLACK_ATTACHMENT_FILENAME_MAX_CHARS: usize = 128;
const SLACK_USER_CACHE_MAX_ENTRIES: usize = 1000;
const SLACK_ATTACHMENT_SAVE_SUBDIR: &str = "slack_files";
const SLACK_ATTACHMENT_MAX_FILES_PER_MESSAGE: usize = 8;

/// Extract the Slack message timestamp from a ZeroClaw message ID.
///
/// Message IDs follow the format `slack_{channel_id}_{ts}` where `ts`
/// contains a dot (e.g. `"1234567890.123456"`). If the format is
/// unrecognised the raw `message_id` is returned as-is.
fn extract_slack_ts(message_id: &str) -> &str {
    message_id
        .strip_prefix("slack_")
        .and_then(|rest| {
            rest.find('.').map(|dot_pos| {
                let underscore = rest[..dot_pos].rfind('_').unwrap_or(0);
                &rest[underscore + 1..]
            })
        })
        .unwrap_or(message_id)
}

/// Map a Unicode emoji to its Slack short-name.
///
/// The orchestration layer passes Unicode characters (e.g. `"\u{1F440}"`).
/// Slack's reactions API expects colon-free short-names (`"eyes"`).
fn unicode_emoji_to_slack_name(emoji: &str) -> &str {
    match emoji {
        "\u{1F440}" => "eyes",                        // 👀
        "\u{2705}" => "white_check_mark",             // ✅
        "\u{26A0}\u{FE0F}" | "\u{26A0}" => "warning", // ⚠️
        "\u{274C}" => "x",                            // ❌
        "\u{1F44D}" => "thumbsup",                    // 👍
        "\u{1F44E}" => "thumbsdown",                  // 👎
        "\u{2B50}" => "star",                         // ⭐
        "\u{1F389}" => "tada",                        // 🎉
        "\u{1F914}" => "thinking_face",               // 🤔
        "\u{1F525}" => "fire",                        // 🔥
        _ => emoji.trim_matches(':'),
    }
}
const SLACK_ATTACHMENT_RENDER_CONCURRENCY: usize = 3;
const SLACK_POLL_ACTIVE_THREAD_MAX: usize = 50;
const SLACK_POLL_THREAD_EXPIRE_SECS: u64 = 24 * 60 * 60;
const SLACK_MEDIA_REDIRECT_MAX_HOPS: usize = 5;
const SLACK_ALLOWED_MEDIA_HOST_SUFFIXES: &[&str] =
    &["slack.com", "slack-edge.com", "slack-files.com"];
const SLACK_SUPPORTED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
];

impl SlackChannel {
    pub fn new(
        bot_token: String,
        app_token: Option<String>,
        channel_id: Option<String>,
        channel_ids: Vec<String>,
        allowed_users: Vec<String>,
    ) -> Self {
        Self {
            bot_token,
            app_token,
            channel_id,
            channel_ids,
            allowed_users,
            thread_replies: true,
            mention_only: false,
            group_reply_allowed_sender_ids: Vec::new(),
            user_display_name_cache: Mutex::new(HashMap::new()),
            workspace_dir: None,
            active_assistant_thread: Mutex::new(HashMap::new()),
            proxy_url: None,
            transcription: None,
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

    /// Configure whether outbound replies stay in the originating Slack thread.
    pub fn with_thread_replies(mut self, thread_replies: bool) -> Self {
        self.thread_replies = thread_replies;
        self
    }

    /// Configure workspace directory used for persisting inbound Slack attachments.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Configure voice transcription for audio file attachments.
    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if config.enabled {
            self.transcription = Some(config);
        }
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_channel_proxy_client_with_timeouts(
            "channel.slack",
            self.proxy_url.as_deref(),
            30,
            10,
        )
    }

    /// Post a new Slack message and return the message timestamp (`ts`).
    ///
    /// This is a lower-level helper that exposes the `ts` value needed for
    /// subsequent `chat.update` calls. For simple sends, use the [`Channel::send`]
    /// trait method instead.
    pub async fn post_message(&self, channel: &str, text: &str) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "channel": channel,
            "text": text,
        });

        let resp = self
            .http_client()
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let raw = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&raw);
            anyhow::bail!("Slack chat.postMessage failed ({status}): {sanitized}");
        }

        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Slack chat.postMessage failed: {err}");
        }

        parsed
            .get("ts")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("Slack chat.postMessage response missing 'ts'"))
    }

    /// Update an existing Slack message in-place using `chat.update`.
    ///
    /// `channel` is the channel ID and `ts` is the timestamp of the original
    /// message (returned by [`post_message`]).
    pub async fn update_message(&self, channel: &str, ts: &str, text: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "channel": channel,
            "ts": ts,
            "text": text,
        });

        let resp = self
            .http_client()
            .post("https://slack.com/api/chat.update")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let raw = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&raw);
            anyhow::bail!("Slack chat.update failed ({status}): {sanitized}");
        }

        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Slack chat.update failed: {err}");
        }

        Ok(())
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

    fn outbound_thread_ts<'a>(&self, message: &'a SendMessage) -> Option<&'a str> {
        if self.thread_replies {
            message.thread_ts.as_deref()
        } else {
            None
        }
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

    /// Like `inbound_thread_ts`, but only returns a value when Slack's own
    /// `thread_ts` field is present (genuine thread reply). Does **not** fall
    /// back to the message's `ts`, so top-level messages get `None`. Used when
    /// `thread_replies=false` so that all top-level messages from the same user
    /// share a single conversation session key.
    fn inbound_thread_ts_genuine_only(msg: &serde_json::Value) -> Option<String> {
        msg.get("thread_ts")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }

    /// Returns the interruption scope identifier for a Slack message.
    ///
    /// Returns `Some(thread_ts)` only when the message is a genuine thread reply
    /// (Slack's `thread_ts` field is present and differs from the message's own `ts`).
    /// Returns `None` for top-level messages and thread parent messages (where
    /// `thread_ts == ts`), placing them in the 3-component scope key
    /// (`channel_reply_target_sender`).
    ///
    /// Intentional: top-level messages and threaded replies are separate conversational
    /// scopes and should not cancel each other's in-flight tasks.
    fn inbound_interruption_scope_id(msg: &serde_json::Value, ts: &str) -> Option<String> {
        msg.get("thread_ts")
            .and_then(|t| t.as_str())
            .filter(|&t| t != ts)
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

    /// Resolve the effective channel scope:
    /// explicit `channel_ids` list first, then single `channel_id`, otherwise wildcard discovery.
    fn scoped_channel_ids(&self) -> Option<Vec<String>> {
        let mut seen = HashSet::new();
        let ids: Vec<String> = self
            .channel_ids
            .iter()
            .filter_map(|entry| Self::normalized_channel_id(Some(entry)))
            .filter(|id| seen.insert(id.clone()))
            .collect();
        if !ids.is_empty() {
            return Some(ids);
        }
        self.configured_channel_id().map(|id| vec![id])
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

    fn user_cache_ttl() -> Duration {
        Duration::from_secs(SLACK_USER_CACHE_TTL_SECS)
    }

    fn sanitize_display_name(name: &str) -> Option<String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn extract_user_display_name(payload: &serde_json::Value) -> Option<String> {
        let user = payload.get("user")?;
        let profile = user.get("profile");

        let candidates = [
            profile
                .and_then(|p| p.get("display_name"))
                .and_then(|v| v.as_str()),
            profile
                .and_then(|p| p.get("display_name_normalized"))
                .and_then(|v| v.as_str()),
            profile
                .and_then(|p| p.get("real_name_normalized"))
                .and_then(|v| v.as_str()),
            profile
                .and_then(|p| p.get("real_name"))
                .and_then(|v| v.as_str()),
            user.get("real_name").and_then(|v| v.as_str()),
            user.get("name").and_then(|v| v.as_str()),
        ];

        for candidate in candidates.into_iter().flatten() {
            if let Some(display_name) = Self::sanitize_display_name(candidate) {
                return Some(display_name);
            }
        }

        None
    }

    fn cached_sender_display_name(&self, user_id: &str) -> Option<String> {
        let now = Instant::now();
        let Ok(mut cache) = self.user_display_name_cache.lock() else {
            return None;
        };

        if let Some(entry) = cache.get(user_id) {
            if now <= entry.expires_at {
                return Some(entry.display_name.clone());
            }
        }

        cache.remove(user_id);
        None
    }

    fn cache_sender_display_name(&self, user_id: &str, display_name: &str) {
        let Ok(mut cache) = self.user_display_name_cache.lock() else {
            return;
        };
        if cache.len() >= SLACK_USER_CACHE_MAX_ENTRIES {
            let now = Instant::now();
            cache.retain(|_, v| v.expires_at > now);
        }
        cache.insert(
            user_id.to_string(),
            CachedSlackDisplayName {
                display_name: display_name.to_string(),
                expires_at: Instant::now() + Self::user_cache_ttl(),
            },
        );
    }

    async fn fetch_sender_display_name(&self, user_id: &str) -> Option<String> {
        let resp = match self
            .http_client()
            .get("https://slack.com/api/users.info")
            .bearer_auth(&self.bot_token)
            .query(&[("user", user_id)])
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                tracing::warn!("Slack users.info request failed for {user_id}: {err}");
                return None;
            }
        };

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&body);
            tracing::warn!("Slack users.info failed for {user_id} ({status}): {sanitized}");
            return None;
        }

        let payload: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if payload.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = payload
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            tracing::warn!("Slack users.info returned error for {user_id}: {err}");
            return None;
        }

        Self::extract_user_display_name(&payload)
    }

    async fn resolve_sender_identity(&self, user_id: &str) -> String {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return String::new();
        }

        if let Some(display_name) = self.cached_sender_display_name(user_id) {
            return display_name;
        }

        if let Some(display_name) = self.fetch_sender_display_name(user_id).await {
            self.cache_sender_display_name(user_id, &display_name);
            return display_name;
        }

        user_id.to_string()
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

    fn normalize_incoming_text(
        text: &str,
        require_mention: bool,
        bot_user_id: &str,
    ) -> Option<String> {
        if require_mention && !Self::contains_bot_mention(text, bot_user_id) {
            return None;
        }

        Some(if require_mention {
            Self::strip_bot_mentions(text, bot_user_id)
        } else {
            text.trim().to_string()
        })
    }

    fn normalize_incoming_content(
        text: &str,
        require_mention: bool,
        bot_user_id: &str,
    ) -> Option<String> {
        let normalized = Self::normalize_incoming_text(text, require_mention, bot_user_id)?;
        if normalized.is_empty() {
            return None;
        }
        Some(normalized)
    }

    fn is_supported_message_subtype(subtype: Option<&str>) -> bool {
        matches!(subtype, None | Some("file_share" | "thread_broadcast"))
    }

    fn compose_incoming_content(text: String, attachment_blocks: Vec<String>) -> Option<String> {
        let mut sections = Vec::new();
        if !text.trim().is_empty() {
            sections.push(text.trim().to_string());
        }
        for block in attachment_blocks {
            if !block.trim().is_empty() {
                sections.push(block);
            }
        }

        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }

    async fn build_incoming_content(
        &self,
        message: &serde_json::Value,
        require_mention: bool,
        bot_user_id: &str,
    ) -> Option<String> {
        let text = message
            .get("text")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let normalized_text = Self::normalize_incoming_text(text, require_mention, bot_user_id)?;
        let attachment_blocks = self.render_file_attachments(message).await;
        Self::compose_incoming_content(normalized_text, attachment_blocks)
    }

    async fn render_file_attachments(&self, message: &serde_json::Value) -> Vec<String> {
        let Some(files) = message.get("files").and_then(|value| value.as_array()) else {
            return Vec::new();
        };

        if files.len() > SLACK_ATTACHMENT_MAX_FILES_PER_MESSAGE {
            tracing::warn!(
                "Slack message has {} files; processing first {} only",
                files.len(),
                SLACK_ATTACHMENT_MAX_FILES_PER_MESSAGE
            );
        }

        let limited_files = files
            .iter()
            .take(SLACK_ATTACHMENT_MAX_FILES_PER_MESSAGE)
            .cloned()
            .collect::<Vec<_>>();

        let tasks =
            limited_files
                .into_iter()
                .enumerate()
                .map(|(idx, raw_file)| async move {
                    (idx, self.render_file_attachment(&raw_file).await)
                });

        let mut rendered = futures_util::stream::iter(tasks)
            .buffer_unordered(SLACK_ATTACHMENT_RENDER_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        rendered.sort_by_key(|(idx, _)| *idx);
        rendered
            .into_iter()
            .filter_map(|(_, block)| block)
            .collect()
    }

    async fn render_file_attachment(&self, raw_file: &serde_json::Value) -> Option<String> {
        let file = self
            .hydrate_file_object(raw_file)
            .await
            .unwrap_or_else(|| raw_file.clone());

        // Voice / audio transcription: if transcription is configured and the
        // file looks like an audio attachment, download and transcribe it.
        if Self::is_audio_file(&file) {
            if let Some(transcribed) = self.try_transcribe_audio_file(&file).await {
                return Some(transcribed);
            }
        }
        if Self::is_image_file(&file) {
            if let Some(marker) = self.fetch_image_marker(&file).await {
                return Some(marker);
            }
        }

        let mut snippet = Self::file_text_preview(&file);
        if snippet.is_none() && Self::is_probably_text_file(&file) {
            snippet = self.download_text_snippet(&file).await;
        }

        if let Some(text) = snippet {
            if !text.trim().is_empty() {
                return Some(Self::format_snippet_attachment(&file, &text));
            }
        }

        Some(Self::format_attachment_summary(&file))
    }

    async fn hydrate_file_object(&self, file: &serde_json::Value) -> Option<serde_json::Value> {
        let file_id = Self::slack_file_id(file)?;
        let file_access = file
            .get("file_access")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let mode = Self::slack_file_mode(file).unwrap_or_default();

        let requires_lookup = file_access.eq_ignore_ascii_case("check_file_info")
            || Self::slack_file_download_url(file).is_none()
            || (Self::is_probably_text_file(file) && Self::file_text_preview(file).is_none())
            || (mode == "snippet" && file.get("preview").is_none());
        if !requires_lookup {
            return Some(file.clone());
        }

        self.fetch_file_info(file_id)
            .await
            .or_else(|| Some(file.clone()))
    }

    async fn fetch_file_info(&self, file_id: &str) -> Option<serde_json::Value> {
        let resp = match self
            .http_client()
            .get("https://slack.com/api/files.info")
            .bearer_auth(&self.bot_token)
            .query(&[("file", file_id)])
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                tracing::warn!("Slack files.info request failed for {file_id}: {err}");
                return None;
            }
        };

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&body);
            tracing::warn!("Slack files.info failed for {file_id} ({status}): {sanitized}");
            return None;
        }

        let payload: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if payload.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = payload
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            tracing::warn!("Slack files.info returned error for {file_id}: {err}");
            return None;
        }

        payload.get("file").cloned()
    }

    fn slack_file_id(file: &serde_json::Value) -> Option<&str> {
        file.get("id").and_then(|value| value.as_str())
    }

    fn slack_file_name(file: &serde_json::Value) -> String {
        file.get("title")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .or_else(|| file.get("name").and_then(|value| value.as_str()))
            .unwrap_or("attachment")
            .trim()
            .to_string()
    }

    fn slack_file_mode(file: &serde_json::Value) -> Option<String> {
        file.get("mode")
            .and_then(|value| value.as_str())
            .map(|value| value.to_ascii_lowercase())
    }

    fn slack_file_mime(file: &serde_json::Value) -> Option<String> {
        file.get("mimetype")
            .and_then(|value| value.as_str())
            .map(|value| value.to_ascii_lowercase())
    }

    fn slack_file_download_url(file: &serde_json::Value) -> Option<&str> {
        file.get("url_private_download")
            .and_then(|value| value.as_str())
            .or_else(|| file.get("url_private").and_then(|value| value.as_str()))
    }

    fn slack_image_candidate_urls(file: &serde_json::Value) -> Vec<String> {
        let mut urls = Vec::new();
        let mut seen = HashSet::new();
        for key in [
            "thumb_1024",
            "thumb_960",
            "thumb_800",
            "thumb_720",
            "thumb_480",
            "thumb_360",
            "thumb_160",
            "url_private_download",
            "url_private",
        ] {
            if let Some(url) = file.get(key).and_then(|value| value.as_str()) {
                let trimmed = url.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if seen.insert(trimmed.to_string()) {
                    urls.push(trimmed.to_string());
                }
            }
        }
        urls
    }

    fn is_allowed_slack_media_hostname(host: &str) -> bool {
        let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if normalized.is_empty() {
            return false;
        }

        SLACK_ALLOWED_MEDIA_HOST_SUFFIXES
            .iter()
            .any(|suffix| normalized == *suffix || normalized.ends_with(&format!(".{suffix}")))
    }

    fn redact_slack_url(url: &reqwest::Url) -> String {
        let host = url.host_str().unwrap_or("unknown-host");
        let tail = url
            .path_segments()
            .and_then(|mut segments| {
                segments
                    .rfind(|segment| !segment.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "root".to_string());
        format!("{host}/.../{tail}")
    }

    fn redact_raw_slack_url(raw_url: &str) -> String {
        reqwest::Url::parse(raw_url)
            .map(|parsed| Self::redact_slack_url(&parsed))
            .unwrap_or_else(|_| "<invalid-url>".to_string())
    }

    fn redact_redirect_location(location: &str) -> String {
        match reqwest::Url::parse(location) {
            Ok(url) => Self::redact_slack_url(&url),
            Err(_) => {
                let tail = location
                    .split(['?', '#'])
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|segment| !segment.is_empty())
                    .unwrap_or("relative");
                format!("relative/.../{tail}")
            }
        }
    }

    fn validate_slack_private_file_url(raw_url: &str) -> Option<reqwest::Url> {
        let parsed = match reqwest::Url::parse(raw_url) {
            Ok(url) => url,
            Err(err) => {
                let redacted_raw = Self::redact_raw_slack_url(raw_url);
                tracing::warn!("Slack file URL parse failed for {redacted_raw}: {err}");
                return None;
            }
        };
        let redacted = Self::redact_slack_url(&parsed);

        if parsed.scheme() != "https" {
            tracing::warn!(
                "Slack file URL rejected due to non-HTTPS scheme for {}: {}",
                redacted,
                parsed.scheme()
            );
            return None;
        }

        let Some(host) = parsed.host_str() else {
            tracing::warn!("Slack file URL rejected due to missing host: {redacted}");
            return None;
        };
        if !Self::is_allowed_slack_media_hostname(host) {
            tracing::warn!("Slack file URL rejected due to non-Slack host: {redacted}");
            return None;
        }

        Some(parsed)
    }

    fn resolve_https_redirect_target(base: &reqwest::Url, location: &str) -> Option<reqwest::Url> {
        let redacted_base = Self::redact_slack_url(base);
        let redacted_location = Self::redact_redirect_location(location);
        let target = match base.join(location) {
            Ok(url) => url,
            Err(err) => {
                tracing::warn!(
                    "Slack file redirect URL parse failed for base {} and location {}: {}",
                    redacted_base,
                    redacted_location,
                    err
                );
                return None;
            }
        };
        let redacted_target = Self::redact_slack_url(&target);
        if target.scheme() != "https" {
            tracing::warn!(
                "Slack file redirect rejected due to non-HTTPS scheme for {}",
                redacted_target
            );
            return None;
        }
        let Some(host) = target.host_str() else {
            tracing::warn!(
                "Slack file redirect rejected due to missing host for {}",
                redacted_target
            );
            return None;
        };
        if !Self::is_allowed_slack_media_hostname(host) {
            tracing::warn!(
                "Slack file redirect rejected due to non-Slack host for {}",
                redacted_target
            );
            return None;
        }
        Some(target)
    }

    fn slack_media_http_client_no_redirect(&self) -> anyhow::Result<reqwest::Client> {
        let builder = crate::config::apply_channel_proxy_to_builder(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(10)),
            "channel.slack",
            self.proxy_url.as_deref(),
        );
        builder
            .build()
            .context("failed to build Slack media no-redirect HTTP client")
    }

    async fn fetch_slack_private_file(&self, raw_url: &str) -> Option<reqwest::Response> {
        let parsed = Self::validate_slack_private_file_url(raw_url)?;
        let redacted_parsed = Self::redact_slack_url(&parsed);
        let client = match self.slack_media_http_client_no_redirect() {
            Ok(client) => client,
            Err(err) => {
                tracing::warn!("Slack file fetch failed for {}: {}", redacted_parsed, err);
                return None;
            }
        };
        let mut current_url = parsed;

        for redirect_hop in 0..=SLACK_MEDIA_REDIRECT_MAX_HOPS {
            let redacted_current = Self::redact_slack_url(&current_url);
            let mut req = client.get(current_url.clone());
            if redirect_hop == 0 {
                req = req.bearer_auth(&self.bot_token);
            }
            let response = match req.send().await {
                Ok(response) => response,
                Err(err) => {
                    tracing::warn!("Slack file fetch failed for {}: {}", redacted_current, err);
                    return None;
                }
            };

            if !response.status().is_redirection() {
                return Some(response);
            }

            if redirect_hop == SLACK_MEDIA_REDIRECT_MAX_HOPS {
                tracing::warn!(
                    "Slack file redirect limit exceeded for {} after {} hops",
                    redacted_current,
                    SLACK_MEDIA_REDIRECT_MAX_HOPS
                );
                return Some(response);
            }

            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                return Some(response);
            };
            let Ok(location) = location.to_str() else {
                tracing::warn!(
                    "Slack file redirect location header is not valid UTF-8 for {}",
                    redacted_current
                );
                return Some(response);
            };
            let Some(next_url) = Self::resolve_https_redirect_target(&current_url, location) else {
                return Some(response);
            };
            current_url = next_url;
        }

        None
    }

    async fn fetch_image_marker(&self, file: &serde_json::Value) -> Option<String> {
        let file_name = Self::slack_file_name(file);
        let image_urls = Self::slack_image_candidate_urls(file);
        if image_urls.is_empty() {
            tracing::warn!(
                "Slack file attachment is image-like but has no downloadable URL: {}",
                file_name
            );
            return None;
        }

        for url in image_urls {
            if let Some(marker) = self.download_private_image_as_marker(&url, file).await {
                return Some(marker);
            }
        }

        tracing::warn!("Slack image attachment download failed for {file_name}");
        None
    }

    async fn download_private_image_as_marker(
        &self,
        url: &str,
        file: &serde_json::Value,
    ) -> Option<String> {
        let redacted_url = Self::redact_raw_slack_url(url);
        let resp = self.fetch_slack_private_file(url).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            let sanitized = crate::providers::sanitize_api_error(&body);
            tracing::warn!(
                "Slack image fetch failed for {} ({status}): {sanitized}",
                redacted_url
            );
            return None;
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if let Some(content_length) = resp.content_length() {
            let content_length = usize::try_from(content_length).unwrap_or(usize::MAX);
            if content_length > SLACK_ATTACHMENT_IMAGE_MAX_BYTES {
                tracing::warn!(
                    "Slack image fetch skipped for {}: content-length {} exceeds {} bytes",
                    redacted_url,
                    content_length,
                    SLACK_ATTACHMENT_IMAGE_MAX_BYTES
                );
                return None;
            }
        }

        let bytes = match resp.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!("Slack image body read failed for {}: {err}", redacted_url);
                return None;
            }
        };
        if bytes.is_empty() {
            tracing::warn!("Slack image body is empty for {}", redacted_url);
            return None;
        }
        if bytes.len() > SLACK_ATTACHMENT_IMAGE_MAX_BYTES {
            tracing::warn!(
                "Slack image body too large for {}: {} bytes exceeds {} bytes",
                redacted_url,
                bytes.len(),
                SLACK_ATTACHMENT_IMAGE_MAX_BYTES
            );
            return None;
        }

        let Some(mime) =
            Self::detect_image_mime(content_type.as_deref(), file, bytes.as_ref(), url)
        else {
            tracing::warn!("Slack image MIME detection failed for {}", redacted_url);
            return None;
        };
        if !Self::is_supported_image_mime(&mime) {
            tracing::warn!(
                "Slack image MIME not supported for {}: {mime}",
                redacted_url
            );
            return None;
        }

        let file_name = Self::slack_file_name(file);
        if let Some(saved_path) = self
            .persist_image_attachment(file, &file_name, &mime, bytes.as_ref())
            .await
        {
            return Some(format!("[IMAGE:{}]", saved_path.display()));
        }

        if bytes.len() > SLACK_ATTACHMENT_IMAGE_INLINE_FALLBACK_MAX_BYTES {
            tracing::warn!(
                "Slack image inline fallback skipped for {}: {} bytes exceeds {} bytes",
                redacted_url,
                bytes.len(),
                SLACK_ATTACHMENT_IMAGE_INLINE_FALLBACK_MAX_BYTES
            );
            return None;
        }

        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Some(format!("[IMAGE:data:{mime};base64,{encoded}]"))
    }

    fn detect_image_mime(
        content_type_header: Option<&str>,
        file: &serde_json::Value,
        bytes: &[u8],
        source_url: &str,
    ) -> Option<String> {
        let redacted_source = Self::redact_raw_slack_url(source_url);
        if let Some(magic_mime) = Self::mime_from_magic(bytes) {
            return Some(magic_mime.to_string());
        }

        if let Some(header_mime) = content_type_header
            .and_then(Self::normalized_content_type)
            .filter(|mime| mime.starts_with("image/"))
        {
            tracing::warn!(
                "Slack image MIME mismatch for {}: HTTP header claims {}, but bytes do not match a supported image signature",
                redacted_source,
                header_mime
            );
        }

        if let Some(file_mime) =
            Self::slack_file_mime(file).filter(|mime| mime.starts_with("image/"))
        {
            tracing::warn!(
                "Slack image MIME mismatch for {}: file metadata claims {}, but bytes do not match a supported image signature",
                redacted_source,
                file_mime
            );
        }

        if let Some(ext) = Self::file_extension(source_url)
            .or_else(|| Self::file_extension(&Self::slack_file_name(file)))
        {
            if let Some(mime) = Self::mime_from_extension(&ext) {
                tracing::warn!(
                    "Slack image MIME mismatch for {}: filename extension implies {}, but bytes do not match a supported image signature",
                    redacted_source,
                    mime
                );
            }
        }

        None
    }

    fn normalized_content_type(content_type: &str) -> Option<String> {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if mime.is_empty() {
            None
        } else {
            Some(mime)
        }
    }

    fn is_supported_image_mime(mime: &str) -> bool {
        SLACK_SUPPORTED_IMAGE_MIME_TYPES.contains(&mime)
    }

    fn mime_from_extension(ext: &str) -> Option<&'static str> {
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            "bmp" => Some("image/bmp"),
            _ => None,
        }
    }

    fn mime_from_magic(bytes: &[u8]) -> Option<&'static str> {
        if bytes.len() >= 8
            && bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
        {
            return Some("image/png");
        }
        if bytes.len() >= 3 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            return Some("image/jpeg");
        }
        if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
            return Some("image/gif");
        }
        if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Some("image/webp");
        }
        if bytes.len() >= 2 && bytes.starts_with(b"BM") {
            return Some("image/bmp");
        }
        None
    }

    async fn persist_image_attachment(
        &self,
        file: &serde_json::Value,
        file_name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Option<PathBuf> {
        let workspace = self.workspace_dir.as_ref()?;
        let safe_name = Self::sanitize_attachment_filename(file_name)
            .unwrap_or_else(|| "attachment".to_string());
        let ext = Self::image_extension_for_mime(mime).unwrap_or("png");
        let safe_name = Self::ensure_file_extension(&safe_name, ext);
        let file_id = Self::slack_file_id(file)
            .map(Self::sanitize_file_id)
            .unwrap_or_else(|| "file".to_string());
        let generated_name = format!(
            "slack_{}_{}_{}",
            Utc::now().timestamp_millis(),
            file_id,
            safe_name
        );

        let output_path = match Self::resolve_workspace_attachment_output_path(
            workspace,
            &generated_name,
        )
        .await
        {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!(
                    "Slack image attachment path resolution failed for {}: {err}",
                    file_name
                );
                return None;
            }
        };

        let Some(parent_dir) = output_path.parent() else {
            tracing::warn!(
                "Slack image attachment write failed for {}: missing parent directory",
                output_path.display()
            );
            return None;
        };

        let file_tail = output_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment");
        let temp_name = format!(
            ".{file_tail}.{}.part",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let temp_path = parent_dir.join(temp_name);

        let mut temp_file = match tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await
        {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(
                    "Slack image attachment temp open failed for {}: {err}",
                    temp_path.display()
                );
                return None;
            }
        };

        if let Err(err) = temp_file.write_all(bytes).await {
            tracing::warn!(
                "Slack image attachment temp write failed for {}: {err}",
                temp_path.display()
            );
            let _ = tokio::fs::remove_file(&temp_path).await;
            return None;
        }
        if let Err(err) = temp_file.sync_all().await {
            tracing::warn!(
                "Slack image attachment temp sync failed for {}: {err}",
                temp_path.display()
            );
            let _ = tokio::fs::remove_file(&temp_path).await;
            return None;
        }
        drop(temp_file);

        // Reject symlinks at the destination to prevent a symlink-following attack
        // where an attacker places a symlink at the target path to redirect writes
        // outside the workspace.
        match tokio::fs::symlink_metadata(&output_path).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                tracing::warn!(
                    "Slack image attachment refused: output path is a symlink: {}",
                    output_path.display()
                );
                let _ = tokio::fs::remove_file(&temp_path).await;
                return None;
            }
            _ => {}
        }

        if let Err(err) = tokio::fs::rename(&temp_path, &output_path).await {
            tracing::warn!(
                "Slack image attachment finalize failed for {}: {err}",
                output_path.display()
            );
            let _ = tokio::fs::remove_file(&temp_path).await;
            return None;
        }

        Some(output_path)
    }

    async fn resolve_workspace_attachment_output_path(
        workspace: &Path,
        file_name: &str,
    ) -> anyhow::Result<PathBuf> {
        let safe_name = Self::sanitize_attachment_filename(file_name)
            .ok_or_else(|| anyhow::anyhow!("invalid attachment filename: {file_name}"))?;

        tokio::fs::create_dir_all(workspace).await?;
        let workspace_root = tokio::fs::canonicalize(workspace)
            .await
            .unwrap_or_else(|_| workspace.to_path_buf());

        let save_dir = workspace.join(SLACK_ATTACHMENT_SAVE_SUBDIR);
        tokio::fs::create_dir_all(&save_dir).await?;
        let resolved_save_dir = tokio::fs::canonicalize(&save_dir).await.with_context(|| {
            format!(
                "failed to resolve Slack attachment save directory: {}",
                save_dir.display()
            )
        })?;

        if !resolved_save_dir.starts_with(&workspace_root) {
            anyhow::bail!(
                "Slack attachment save directory escapes workspace: {}",
                resolved_save_dir.display()
            );
        }

        Ok(resolved_save_dir.join(safe_name))
    }

    fn sanitize_attachment_filename(file_name: &str) -> Option<String> {
        let basename = Path::new(file_name).file_name()?.to_str()?.trim();
        if basename.is_empty() || basename == "." || basename == ".." {
            return None;
        }

        let sanitized: String = basename
            .replace(['/', '\\'], "_")
            .chars()
            .take(SLACK_ATTACHMENT_FILENAME_MAX_CHARS)
            .collect();
        if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
            None
        } else {
            Some(sanitized)
        }
    }

    fn sanitize_file_id(file_id: &str) -> String {
        let cleaned: String = file_id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
            .take(64)
            .collect();
        if cleaned.is_empty() {
            "file".to_string()
        } else {
            cleaned
        }
    }

    fn ensure_file_extension(file_name: &str, extension: &str) -> String {
        if Path::new(file_name).extension().is_some() {
            file_name.to_string()
        } else {
            format!("{file_name}.{extension}")
        }
    }

    fn image_extension_for_mime(mime: &str) -> Option<&'static str> {
        match mime {
            "image/png" => Some("png"),
            "image/jpeg" => Some("jpg"),
            "image/webp" => Some("webp"),
            "image/gif" => Some("gif"),
            "image/bmp" => Some("bmp"),
            _ => None,
        }
    }

    fn file_extension(value: &str) -> Option<String> {
        let before_query = value.split('?').next().unwrap_or(value);
        before_query
            .rsplit('/')
            .next()
            .unwrap_or(before_query)
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
    }

    fn file_text_preview(file: &serde_json::Value) -> Option<String> {
        let preview = file
            .get("preview")
            .and_then(|value| value.as_str())
            .or_else(|| {
                file.get("preview_highlight")
                    .and_then(|value| value.as_str())
            })
            .or_else(|| {
                file.get("initial_comment")
                    .and_then(|comment| comment.get("comment"))
                    .and_then(|value| value.as_str())
            })?;
        Self::truncate_text(preview, SLACK_ATTACHMENT_TEXT_INLINE_MAX_CHARS)
    }

    fn truncate_text(value: &str, max_chars: usize) -> Option<String> {
        let mut out = String::new();
        let mut count = 0usize;
        for ch in value.chars() {
            if count >= max_chars {
                break;
            }
            out.push(ch);
            count += 1;
        }
        let was_truncated = count >= max_chars && value.chars().nth(max_chars).is_some();
        let mut out = out.trim().to_string();
        if out.is_empty() {
            return None;
        }
        if was_truncated {
            out.push_str("\n…[truncated]");
        }
        Some(out)
    }

    fn is_probably_text_file(file: &serde_json::Value) -> bool {
        if matches!(
            Self::slack_file_mode(file).as_deref(),
            Some("snippet" | "post")
        ) {
            return true;
        }

        if Self::slack_file_mime(file)
            .as_deref()
            .is_some_and(|mime| mime.starts_with("text/"))
        {
            return true;
        }

        if file
            .get("filetype")
            .and_then(|value| value.as_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
            .is_some_and(Self::is_text_filetype)
        {
            return true;
        }

        Self::file_extension(&Self::slack_file_name(file))
            .as_deref()
            .is_some_and(Self::is_text_filetype)
    }

    fn is_text_filetype(filetype: &str) -> bool {
        matches!(
            filetype,
            "txt"
                | "text"
                | "md"
                | "markdown"
                | "csv"
                | "tsv"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "xml"
                | "html"
                | "css"
                | "js"
                | "ts"
                | "jsx"
                | "tsx"
                | "py"
                | "rs"
                | "go"
                | "java"
                | "kt"
                | "c"
                | "cc"
                | "cpp"
                | "h"
                | "hpp"
                | "cs"
                | "php"
                | "rb"
                | "swift"
                | "sql"
                | "log"
                | "ini"
                | "conf"
                | "cfg"
                | "env"
                | "sh"
                | "bash"
                | "zsh"
        )
    }

    fn is_image_file(file: &serde_json::Value) -> bool {
        if Self::slack_file_mime(file)
            .as_deref()
            .is_some_and(|mime| mime.starts_with("image/"))
        {
            return true;
        }

        if file
            .get("filetype")
            .and_then(|value| value.as_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
            .is_some_and(|filetype| Self::mime_from_extension(filetype).is_some())
        {
            return true;
        }

        Self::file_extension(&Self::slack_file_name(file))
            .as_deref()
            .is_some_and(|ext| Self::mime_from_extension(ext).is_some())
    }

    /// Audio file extensions accepted for voice transcription.
    const AUDIO_EXTENSIONS: &[&str] = &[
        "flac", "mp3", "mpeg", "mpga", "mp4", "m4a", "ogg", "oga", "opus", "wav", "webm",
    ];

    /// Check whether a Slack file object looks like an audio attachment
    /// (voice memo, audio message, or uploaded audio file).
    fn is_audio_file(file: &serde_json::Value) -> bool {
        // Slack voice messages use subtype "slack_audio"
        if let Some(subtype) = file.get("subtype").and_then(|v| v.as_str()) {
            if subtype == "slack_audio" {
                return true;
            }
        }

        if Self::slack_file_mime(file)
            .as_deref()
            .is_some_and(|mime| mime.starts_with("audio/"))
        {
            return true;
        }

        if let Some(ft) = file
            .get("filetype")
            .and_then(|v| v.as_str())
            .map(|v| v.to_ascii_lowercase())
        {
            if Self::AUDIO_EXTENSIONS.contains(&ft.as_str()) {
                return true;
            }
        }

        Self::file_extension(&Self::slack_file_name(file))
            .as_deref()
            .is_some_and(|ext| Self::AUDIO_EXTENSIONS.contains(&ext))
    }

    /// Download an audio file attachment and transcribe it using the configured
    /// transcription provider. Returns `None` if transcription is not configured
    /// or if the download/transcription fails.
    async fn try_transcribe_audio_file(&self, file: &serde_json::Value) -> Option<String> {
        let config = self.transcription.as_ref()?;

        let url = Self::slack_file_download_url(file)?;
        let file_name = Self::slack_file_name(file);
        let redacted_url = Self::redact_raw_slack_url(url);

        let resp = self.fetch_slack_private_file(url).await?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(
                "Slack voice file download failed for {} ({status})",
                redacted_url
            );
            return None;
        }

        let audio_data = match resp.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                tracing::warn!("Slack voice file read failed for {}: {e}", redacted_url);
                return None;
            }
        };

        // Determine a filename with extension for the transcription API.
        let transcription_filename = if Self::file_extension(&file_name).is_some() {
            file_name.clone()
        } else {
            // Fall back to extension from mimetype or default to .ogg
            let mime_ext = Self::slack_file_mime(file)
                .and_then(|mime| mime.rsplit('/').next().map(|s| s.to_string()))
                .unwrap_or_else(|| "ogg".to_string());
            format!("voice.{mime_ext}")
        };

        match super::transcription::transcribe_audio(audio_data, &transcription_filename, config)
            .await
        {
            Ok(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    tracing::info!("Slack voice transcription returned empty text, skipping");
                    None
                } else {
                    tracing::info!(
                        "Slack: transcribed voice file {} ({} chars)",
                        file_name,
                        trimmed.len()
                    );
                    Some(format!("[Voice] {trimmed}"))
                }
            }
            Err(e) => {
                tracing::warn!("Slack voice transcription failed for {}: {e}", file_name);
                Some(Self::format_attachment_summary(file))
            }
        }
    }

    async fn download_text_snippet(&self, file: &serde_json::Value) -> Option<String> {
        let url = Self::slack_file_download_url(file)?;
        let redacted_url = Self::redact_raw_slack_url(url);
        let resp = self.fetch_slack_private_file(url).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            let sanitized = crate::providers::sanitize_api_error(&body);
            tracing::warn!(
                "Slack snippet fetch failed for {} ({status}): {sanitized}",
                redacted_url
            );
            return None;
        }

        if let Some(content_length) = resp.content_length() {
            let content_length = usize::try_from(content_length).unwrap_or(usize::MAX);
            if content_length > SLACK_ATTACHMENT_TEXT_DOWNLOAD_MAX_BYTES {
                tracing::warn!(
                    "Slack snippet download skipped for {}: content-length {} exceeds {} bytes",
                    redacted_url,
                    content_length,
                    SLACK_ATTACHMENT_TEXT_DOWNLOAD_MAX_BYTES
                );
                return None;
            }
        }

        let bytes = match resp.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!("Slack snippet body read failed for {}: {err}", redacted_url);
                return None;
            }
        };
        if bytes.is_empty() {
            return None;
        }
        if bytes.len() > SLACK_ATTACHMENT_TEXT_DOWNLOAD_MAX_BYTES {
            tracing::warn!(
                "Slack snippet body too large for {}: {} bytes exceeds {} bytes",
                redacted_url,
                bytes.len(),
                SLACK_ATTACHMENT_TEXT_DOWNLOAD_MAX_BYTES
            );
            return None;
        }
        if bytes.contains(&0) {
            tracing::warn!("Slack snippet body appears binary for {}", redacted_url);
            return None;
        }

        let text = String::from_utf8_lossy(&bytes);
        Self::truncate_text(&text, SLACK_ATTACHMENT_TEXT_INLINE_MAX_CHARS)
    }

    fn format_snippet_attachment(file: &serde_json::Value, snippet: &str) -> String {
        let file_name = Self::slack_file_name(file);
        let language = file
            .get("filetype")
            .and_then(|value| value.as_str())
            .map(Self::sanitize_code_fence_language)
            .unwrap_or_else(|| "text".to_string());

        let fence = if snippet.contains("```") {
            "````"
        } else {
            "```"
        };
        format!("[SNIPPET:{file_name}]\n{fence}{language}\n{snippet}\n{fence}")
    }

    fn sanitize_code_fence_language(input: &str) -> String {
        let normalized = input
            .trim()
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+'))
            .collect::<String>();
        if normalized.is_empty() {
            "text".to_string()
        } else {
            normalized
        }
    }

    fn format_attachment_summary(file: &serde_json::Value) -> String {
        let file_name = Self::slack_file_name(file);
        let mime = Self::slack_file_mime(file).unwrap_or_else(|| "unknown".to_string());
        let size = file
            .get("size")
            .and_then(|value| value.as_u64())
            .map(|value| format!("{value} bytes"))
            .unwrap_or_else(|| "unknown size".to_string());
        format!("[ATTACHMENT:{file_name} | mime={mime} | size={size}]")
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

    /// Parse a Socket Mode `interactive` envelope containing a `block_actions`
    /// payload from the `/config` Block Kit UI.  Translates provider/model
    /// dropdown selections into synthetic `/models <provider>` or `/model <id>`
    /// commands so the existing runtime command handler can apply them.
    fn parse_block_action_as_command(
        envelope: &serde_json::Value,
        _bot_user_id: &str,
    ) -> Option<ChannelMessage> {
        let payload = envelope.get("payload")?;

        let payload_type = payload.get("type").and_then(|v| v.as_str())?;
        if payload_type != "block_actions" {
            return None;
        }

        let actions = payload.get("actions").and_then(|v| v.as_array())?;
        let action = actions.first()?;

        let action_id = action.get("action_id").and_then(|v| v.as_str())?;
        let selected_value = action
            .get("selected_option")
            .and_then(|o| o.get("value"))
            .and_then(|v| v.as_str())?;

        let command = match action_id {
            "zeroclaw_config_provider" => format!("/models {selected_value}"),
            "zeroclaw_config_model" => format!("/model {selected_value}"),
            _ => return None,
        };

        let user = payload
            .get("user")
            .and_then(|u| u.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let channel_id = payload
            .get("channel")
            .and_then(|c| c.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if channel_id.is_empty() {
            tracing::warn!("Slack block_actions: missing channel ID in interactive payload");
            return None;
        }

        let ts = payload
            .get("message")
            .and_then(|m| m.get("ts"))
            .and_then(|v| v.as_str())
            .unwrap_or("0");

        Some(ChannelMessage {
            id: format!("slack_{channel_id}_{ts}_action"),
            sender: user.to_string(),
            reply_target: channel_id.to_string(),
            content: command,
            channel: "slack".to_string(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: payload
                .get("message")
                .and_then(|m| m.get("thread_ts"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            interruption_scope_id: None,
            attachments: vec![],
        })
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
        scoped_channels: Option<Vec<String>>,
    ) -> anyhow::Result<()> {
        let mut last_ts_by_channel: HashMap<String, String> = HashMap::new();
        let mut open_url_attempt: u32 = 0;
        let mut socket_reconnect_attempt: u32 = 0;

        loop {
            let ws_url = match self.open_socket_mode_url().await {
                Ok(url) => {
                    open_url_attempt = 0;
                    url
                }
                Err(e) => {
                    let wait = Self::compute_socket_mode_retry_delay(open_url_attempt);
                    tracing::warn!(
                        "Slack Socket Mode: failed to open websocket URL: {e}; retrying in {:.3}s (attempt #{})",
                        wait.as_secs_f64(),
                        open_url_attempt.saturating_add(1),
                    );
                    open_url_attempt = open_url_attempt.saturating_add(1);
                    tokio::time::sleep(wait).await;
                    continue;
                }
            };

            let (ws_stream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
                Ok(connection) => {
                    socket_reconnect_attempt = 0;
                    connection
                }
                Err(e) => {
                    let wait = Self::compute_socket_mode_retry_delay(socket_reconnect_attempt);
                    tracing::warn!(
                        "Slack Socket Mode: websocket connect failed: {e}; retrying in {:.3}s (attempt #{})",
                        wait.as_secs_f64(),
                        socket_reconnect_attempt.saturating_add(1),
                    );
                    socket_reconnect_attempt = socket_reconnect_attempt.saturating_add(1);
                    tokio::time::sleep(wait).await;
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

                // Handle interactive payloads (block_actions from /config UI).
                if envelope_type == "interactive" {
                    if let Some(msg) = Self::parse_block_action_as_command(&envelope, bot_user_id) {
                        if tx.send(msg).await.is_err() {
                            return Ok(());
                        }
                    }
                    continue;
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
                let event_type = event
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                // Track assistant thread context for Assistants API status indicators.
                if event_type == "assistant_thread_started"
                    || event_type == "assistant_thread_context_changed"
                {
                    if let Some(thread) = event.get("assistant_thread") {
                        let ch = thread
                            .get("channel_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        let tts = thread
                            .get("thread_ts")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        if !ch.is_empty() && !tts.is_empty() {
                            if let Ok(mut map) = self.active_assistant_thread.lock() {
                                map.insert(ch.to_string(), tts.to_string());
                            }
                        }
                    }
                    continue;
                }

                if event_type != "message" {
                    continue;
                }
                let subtype = event.get("subtype").and_then(|v| v.as_str());
                if !Self::is_supported_message_subtype(subtype) {
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
                if let Some(ref configured_channels) = scoped_channels {
                    if !configured_channels.iter().any(|id| id == &channel_id) {
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

                let Some(normalized_text) = self
                    .build_incoming_content(event, require_mention, bot_user_id)
                    .await
                else {
                    continue;
                };

                last_ts_by_channel.insert(channel_id.clone(), ts.to_string());
                let sender = self.resolve_sender_identity(user).await;

                let channel_msg = ChannelMessage {
                    id: format!("slack_{channel_id}_{ts}"),
                    sender,
                    reply_target: channel_id.clone(),
                    content: normalized_text,
                    channel: "slack".to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts: if self.thread_replies {
                        Self::inbound_thread_ts(event, ts)
                    } else {
                        Self::inbound_thread_ts_genuine_only(event)
                    },
                    interruption_scope_id: Self::inbound_interruption_scope_id(event, ts),
                    attachments: vec![],
                };

                // Track thread context so start_typing can set assistant status.
                if let Some(ref tts) = channel_msg.thread_ts {
                    if let Ok(mut map) = self.active_assistant_thread.lock() {
                        map.insert(channel_id.clone(), tts.clone());
                    }
                }

                if tx.send(channel_msg).await.is_err() {
                    return Ok(());
                }
            }

            let wait = Self::compute_socket_mode_retry_delay(socket_reconnect_attempt);
            tracing::warn!(
                "Slack Socket Mode: reconnecting in {:.3}s (attempt #{})...",
                wait.as_secs_f64(),
                socket_reconnect_attempt.saturating_add(1),
            );
            socket_reconnect_attempt = socket_reconnect_attempt.saturating_add(1);
            tokio::time::sleep(wait).await;
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

    fn jitter_ms(max_jitter_ms: u64) -> u64 {
        if max_jitter_ms == 0 {
            return 0;
        }
        rand::random::<u64>() % (max_jitter_ms + 1)
    }

    fn compute_exponential_backoff_delay(
        base_retry_after_secs: u64,
        attempt: u32,
        max_backoff_secs: u64,
        jitter_ms: u64,
    ) -> Duration {
        let multiplier = 1_u64.checked_shl(attempt).unwrap_or(u64::MAX);
        let backoff_secs = base_retry_after_secs
            .saturating_mul(multiplier)
            .min(max_backoff_secs);
        Duration::from_secs(backoff_secs) + Duration::from_millis(jitter_ms)
    }

    fn compute_retry_delay(base_retry_after_secs: u64, attempt: u32, jitter_ms: u64) -> Duration {
        Self::compute_exponential_backoff_delay(
            base_retry_after_secs,
            attempt,
            SLACK_HISTORY_MAX_BACKOFF_SECS,
            jitter_ms,
        )
    }

    fn compute_socket_mode_retry_delay(attempt: u32) -> Duration {
        let jitter_ms = Self::jitter_ms(SLACK_SOCKET_MODE_MAX_JITTER_MS);
        Self::compute_exponential_backoff_delay(
            SLACK_SOCKET_MODE_INITIAL_BACKOFF_SECS,
            attempt,
            SLACK_SOCKET_MODE_MAX_BACKOFF_SECS,
            jitter_ms,
        )
    }

    fn next_retry_timestamp(wait: Duration) -> String {
        match chrono::Duration::from_std(wait) {
            Ok(delta) => (Utc::now() + delta).to_rfc3339(),
            Err(_) => Utc::now().to_rfc3339(),
        }
    }

    fn evaluate_health(bot_ok: bool, socket_mode_enabled: bool, socket_mode_ok: bool) -> bool {
        if !bot_ok {
            return false;
        }
        if socket_mode_enabled {
            return socket_mode_ok;
        }
        true
    }

    fn slack_api_call_succeeded(status: reqwest::StatusCode, body: &str) -> bool {
        if !status.is_success() {
            return false;
        }

        let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
        parsed
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
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
                let jitter_ms = Self::jitter_ms(SLACK_HISTORY_MAX_JITTER_MS);
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

    async fn fetch_thread_replies_with_retry(
        &self,
        channel_id: &str,
        thread_ts: &str,
        oldest: &str,
    ) -> Option<serde_json::Value> {
        let mut total_wait = Duration::from_secs(0);

        for attempt in 0..=SLACK_HISTORY_MAX_RETRIES {
            let resp = match self
                .http_client()
                .get("https://slack.com/api/conversations.replies")
                .bearer_auth(&self.bot_token)
                .query(&[
                    ("channel", channel_id),
                    ("ts", thread_ts),
                    ("oldest", oldest),
                    ("limit", "50"),
                ])
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "Slack conversations.replies error for thread {thread_ts} in {channel_id}: {e}"
                    );
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
                        "Slack rate limit retries exhausted for conversations.replies on thread {} in channel {}. Total wait: {}s across {} attempts.",
                        thread_ts,
                        channel_id,
                        total_wait.as_secs(),
                        SLACK_HISTORY_MAX_RETRIES
                    );
                    return None;
                }

                let retry_after_secs = Self::parse_retry_after_secs(&headers)
                    .unwrap_or(SLACK_HISTORY_DEFAULT_RETRY_AFTER_SECS);
                let jitter_ms = Self::jitter_ms(SLACK_HISTORY_MAX_JITTER_MS);
                let wait = Self::compute_retry_delay(retry_after_secs, attempt, jitter_ms);
                total_wait += wait;
                let next_retry_at = Self::next_retry_timestamp(wait);
                tracing::warn!(
                    "Slack conversations.replies rate limited for thread {} in channel {}. Retry-After: {}s. Attempt {}/{}. Next retry at {}.",
                    thread_ts,
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
                    "Slack conversations.replies failed for thread {} in channel {} ({}): {}",
                    thread_ts,
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
                tracing::warn!(
                    "Slack conversations.replies error for thread {} in channel {}: {}",
                    thread_ts,
                    channel_id,
                    err
                );
                return None;
            }

            return Some(payload);
        }

        None
    }

    /// Extract thread parent timestamps from channel history messages.
    /// Returns `(thread_ts, latest_reply_ts)` pairs for messages with active threads.
    fn extract_active_threads(messages: &[serde_json::Value]) -> Vec<(String, String)> {
        messages
            .iter()
            .filter_map(|msg| {
                let thread_ts = msg.get("thread_ts").and_then(|v| v.as_str())?;
                let ts = msg.get("ts").and_then(|v| v.as_str()).unwrap_or_default();
                // Only consider messages that are thread parents (ts == thread_ts)
                if ts != thread_ts {
                    return None;
                }
                let reply_count = msg.get("reply_count").and_then(|v| v.as_u64()).unwrap_or(0);
                if reply_count == 0 {
                    return None;
                }
                let latest_reply = msg
                    .get("latest_reply")
                    .and_then(|v| v.as_str())
                    .unwrap_or(thread_ts);
                Some((thread_ts.to_string(), latest_reply.to_string()))
            })
            .collect()
    }

    /// Evict expired or excess threads from the active-thread tracker.
    /// Each value is `(channel_id, last_seen_reply_ts, last_activity)`.
    fn evict_stale_threads(
        active_threads: &mut HashMap<String, (String, String, Instant)>,
        now: Instant,
    ) {
        let max_age = Duration::from_secs(SLACK_POLL_THREAD_EXPIRE_SECS);
        active_threads
            .retain(|_, (_, _, last_activity)| now.duration_since(*last_activity) < max_age);
        if active_threads.len() > SLACK_POLL_ACTIVE_THREAD_MAX {
            let overflow = active_threads.len() - SLACK_POLL_ACTIVE_THREAD_MAX;
            let mut entries: Vec<_> = active_threads
                .iter()
                .map(|(k, (_, _, t))| (k.clone(), *t))
                .collect();
            entries.sort_by_key(|(_, t)| *t);
            for (key, _) in entries.into_iter().take(overflow) {
                active_threads.remove(&key);
            }
        }
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Detect Block Kit payloads produced by the `/config` command.
        let body = if let Some(blocks_json) = message.content.strip_prefix(super::BLOCK_KIT_PREFIX)
        {
            let blocks: serde_json::Value = serde_json::from_str(blocks_json)
                .context("invalid Block Kit JSON in runtime command response")?;
            let mut body = serde_json::json!({
                "channel": message.recipient,
                "text": "Model configuration",
                "blocks": blocks
            });
            if let Some(ts) = self.outbound_thread_ts(message) {
                body["thread_ts"] = serde_json::json!(ts);
            }
            body
        } else {
            let mut body = serde_json::json!({
                "channel": message.recipient,
                "text": message.content
            });

            // Use Slack's native markdown block for rich formatting when content fits.
            if message.content.len() <= SLACK_MARKDOWN_BLOCK_MAX_CHARS {
                body["blocks"] = serde_json::json!([{
                    "type": "markdown",
                    "text": message.content
                }]);
            }

            if let Some(ts) = self.outbound_thread_ts(message) {
                body["thread_ts"] = serde_json::json!(ts);
            }
            body
        };

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

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let ts = extract_slack_ts(message_id);
        let name = unicode_emoji_to_slack_name(emoji);

        let body = serde_json::json!({
            "channel": channel_id,
            "timestamp": ts,
            "name": name
        });

        let resp = self
            .http_client()
            .post("https://slack.com/api/reactions.add")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&text);
            anyhow::bail!("Slack reactions.add failed ({status}): {sanitized}");
        }

        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            if err != "already_reacted" {
                anyhow::bail!("Slack reactions.add failed: {err}");
            }
        }

        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let ts = extract_slack_ts(message_id);
        let name = unicode_emoji_to_slack_name(emoji);

        let body = serde_json::json!({
            "channel": channel_id,
            "timestamp": ts,
            "name": name
        });

        let resp = self
            .http_client()
            .post("https://slack.com/api/reactions.remove")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&text);
            anyhow::bail!("Slack reactions.remove failed ({status}): {sanitized}");
        }

        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            if err != "no_reaction" {
                anyhow::bail!("Slack reactions.remove failed: {err}");
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = self.get_bot_user_id().await.unwrap_or_default();
        let scoped_channels = self.scoped_channel_ids();
        if self.configured_app_token().is_some() {
            tracing::info!("Slack channel listening in Socket Mode");
            return self
                .listen_socket_mode(tx, &bot_user_id, scoped_channels)
                .await;
        }

        let mut discovered_channels: Vec<String> = Vec::new();
        let mut last_discovery = Instant::now();
        let mut last_ts_by_channel: HashMap<String, String> = HashMap::new();
        // Active thread tracker: thread_ts -> (channel_id, last_seen_reply_ts, last_activity)
        let mut active_threads: HashMap<String, (String, String, Instant)> = HashMap::new();

        if let Some(ref channel_ids) = scoped_channels {
            tracing::info!(
                "Slack channel listening on {} configured channel(s): {}",
                channel_ids.len(),
                channel_ids.join(", ")
            );
        } else {
            tracing::info!(
                "Slack channel_id/channel_ids not set (or wildcard only); listening across all accessible channels."
            );
        }

        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;

            let target_channels = if let Some(ref channel_ids) = scoped_channels {
                channel_ids.clone()
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
                    // Register thread parents discovered in channel history.
                    for (thread_ts, latest_reply) in Self::extract_active_threads(messages) {
                        let entry = active_threads.entry(thread_ts.clone()).or_insert_with(|| {
                            (channel_id.clone(), thread_ts.clone(), Instant::now())
                        });
                        if latest_reply > entry.1 {
                            entry.1 = latest_reply;
                        }
                        entry.2 = Instant::now();
                    }

                    // Messages come newest-first, reverse to process oldest first
                    for msg in messages.iter().rev() {
                        let subtype = msg.get("subtype").and_then(|value| value.as_str());
                        if !Self::is_supported_message_subtype(subtype) {
                            continue;
                        }
                        let ts = msg.get("ts").and_then(|t| t.as_str()).unwrap_or("");
                        let user = msg
                            .get("user")
                            .and_then(|u| u.as_str())
                            .unwrap_or("unknown");
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

                        if ts <= last_ts {
                            continue;
                        }

                        let is_group_message = Self::is_group_channel_id(&channel_id);
                        let allow_sender_without_mention =
                            is_group_message && self.is_group_sender_trigger_enabled(user);
                        let require_mention =
                            self.mention_only && is_group_message && !allow_sender_without_mention;
                        let Some(normalized_text) = self
                            .build_incoming_content(msg, require_mention, &bot_user_id)
                            .await
                        else {
                            continue;
                        };

                        last_ts_by_channel.insert(channel_id.clone(), ts.to_string());
                        let sender = self.resolve_sender_identity(user).await;

                        let channel_msg = ChannelMessage {
                            id: format!("slack_{channel_id}_{ts}"),
                            sender,
                            reply_target: channel_id.clone(),
                            content: normalized_text,
                            channel: "slack".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            thread_ts: if self.thread_replies {
                                Self::inbound_thread_ts(msg, ts)
                            } else {
                                Self::inbound_thread_ts_genuine_only(msg)
                            },
                            interruption_scope_id: Self::inbound_interruption_scope_id(msg, ts),
                            attachments: vec![],
                        };

                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }

            // Poll active threads for new replies via conversations.replies.
            Self::evict_stale_threads(&mut active_threads, Instant::now());
            let thread_snapshot: Vec<(String, String, String)> = active_threads
                .iter()
                .map(|(thread_ts, (ch, last_reply, _))| {
                    (thread_ts.clone(), ch.clone(), last_reply.clone())
                })
                .collect();

            for (thread_ts, thread_channel_id, last_reply_ts) in thread_snapshot {
                let Some(data) = self
                    .fetch_thread_replies_with_retry(&thread_channel_id, &thread_ts, &last_reply_ts)
                    .await
                else {
                    continue;
                };

                let Some(replies) = data.get("messages").and_then(|m| m.as_array()) else {
                    continue;
                };

                for reply in replies {
                    let reply_ts = reply.get("ts").and_then(|v| v.as_str()).unwrap_or_default();
                    if reply_ts.is_empty() || reply_ts <= last_reply_ts.as_str() {
                        continue;
                    }
                    let subtype = reply.get("subtype").and_then(|v| v.as_str());
                    if !Self::is_supported_message_subtype(subtype) {
                        continue;
                    }

                    let user = reply
                        .get("user")
                        .and_then(|u| u.as_str())
                        .unwrap_or_default();
                    if user.is_empty() || user == bot_user_id {
                        continue;
                    }
                    if !self.is_user_allowed(user) {
                        continue;
                    }

                    let is_group_message = Self::is_group_channel_id(&thread_channel_id);
                    let allow_sender_without_mention =
                        is_group_message && self.is_group_sender_trigger_enabled(user);
                    let require_mention =
                        self.mention_only && is_group_message && !allow_sender_without_mention;
                    let Some(normalized_text) = self
                        .build_incoming_content(reply, require_mention, &bot_user_id)
                        .await
                    else {
                        continue;
                    };

                    // Update the last-seen reply ts for this thread.
                    if let Some(entry) = active_threads.get_mut(&thread_ts) {
                        if reply_ts > entry.1.as_str() {
                            entry.1 = reply_ts.to_string();
                        }
                        entry.2 = Instant::now();
                    }

                    let sender = self.resolve_sender_identity(user).await;

                    let channel_msg = ChannelMessage {
                        id: format!("slack_{thread_channel_id}_{reply_ts}"),
                        sender,
                        reply_target: thread_channel_id.clone(),
                        content: normalized_text,
                        channel: "slack".to_string(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: Some(thread_ts.clone()),
                        interruption_scope_id: Some(thread_ts.clone()),
                        attachments: vec![],
                    };

                    if tx.send(channel_msg).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let bot_ok = match self
            .http_client()
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                Self::slack_api_call_succeeded(status, &body)
            }
            Err(_) => false,
        };
        let socket_mode_enabled = self.configured_app_token().is_some();
        let socket_mode_ok = if socket_mode_enabled {
            self.open_socket_mode_url().await.is_ok()
        } else {
            true
        };
        Self::evaluate_health(bot_ok, socket_mode_enabled, socket_mode_ok)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let thread_ts = {
            let map = self
                .active_assistant_thread
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            match map.get(recipient) {
                Some(ts) => ts.clone(),
                None => return Ok(()),
            }
        };

        let body = serde_json::json!({
            "channel_id": recipient,
            "thread_ts": thread_ts,
            "status": "is thinking...",
        });

        // Gracefully ignore errors — non-assistant contexts will return errors.
        if let Ok(resp) = self
            .http_client()
            .post("https://slack.com/api/assistant.threads.setStatus")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await
        {
            if !resp.status().is_success() {
                tracing::debug!(
                    "assistant.threads.setStatus returned {}; ignoring",
                    resp.status()
                );
            }
        }

        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // Status auto-clears when the bot sends a message via chat.postMessage.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_channel_name() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![]);
        assert_eq!(ch.name(), "slack");
    }

    #[test]
    fn slack_channel_with_channel_id() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            None,
            Some("C12345".into()),
            vec![],
            vec![],
        );
        assert_eq!(ch.channel_id, Some("C12345".to_string()));
    }

    #[test]
    fn slack_group_reply_policy_defaults_to_all_messages() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["*".into()]);
        assert!(ch.thread_replies);
        assert!(!ch.mention_only);
        assert!(ch.group_reply_allowed_sender_ids.is_empty());
    }

    #[test]
    fn with_thread_replies_sets_flag() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![])
            .with_thread_replies(false);
        assert!(!ch.thread_replies);
    }

    #[test]
    fn outbound_thread_ts_respects_thread_replies_setting() {
        let msg = SendMessage::new("hello", "C123").in_thread(Some("1741234567.100001".into()));

        let threaded = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![]);
        assert_eq!(threaded.outbound_thread_ts(&msg), Some("1741234567.100001"));

        let channel_root = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![])
            .with_thread_replies(false);
        assert_eq!(channel_root.outbound_thread_ts(&msg), None);
    }

    #[test]
    fn with_workspace_dir_sets_field() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![])
            .with_workspace_dir(PathBuf::from("/tmp/slack-workspace"));
        assert_eq!(
            ch.workspace_dir.as_deref(),
            Some(std::path::Path::new("/tmp/slack-workspace"))
        );
    }

    #[test]
    fn slack_group_reply_policy_applies_sender_overrides() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["*".into()])
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
        let ch = SlackChannel::new("xoxb-fake".into(), Some("   ".into()), None, vec![], vec![]);
        assert_eq!(ch.configured_app_token(), None);
    }

    #[test]
    fn configured_app_token_trims_value() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            Some(" xapp-123 ".into()),
            None,
            vec![],
            vec![],
        );
        assert_eq!(ch.configured_app_token().as_deref(), Some("xapp-123"));
    }

    #[test]
    fn scoped_channel_ids_prefers_explicit_list() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            None,
            Some("C_SINGLE".into()),
            vec!["C_LIST1".into(), "D_DM1".into()],
            vec![],
        );
        assert_eq!(
            ch.scoped_channel_ids(),
            Some(vec!["C_LIST1".to_string(), "D_DM1".to_string()])
        );
    }

    #[test]
    fn scoped_channel_ids_falls_back_to_single_channel_id() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            None,
            Some("C_SINGLE".into()),
            vec![],
            vec![],
        );
        assert_eq!(ch.scoped_channel_ids(), Some(vec!["C_SINGLE".to_string()]));
    }

    #[test]
    fn scoped_channel_ids_returns_none_for_wildcard_mode() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![]);
        assert_eq!(ch.scoped_channel_ids(), None);
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
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![]);
        assert!(!ch.is_user_allowed("U12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["*".into()]);
        assert!(ch.is_user_allowed("U12345"));
    }

    #[test]
    fn extract_user_display_name_prefers_profile_display_name() {
        let payload = serde_json::json!({
            "ok": true,
            "user": {
                "name": "fallback_name",
                "profile": {
                    "display_name": "Display Name",
                    "real_name": "Real Name"
                }
            }
        });

        assert_eq!(
            SlackChannel::extract_user_display_name(&payload).as_deref(),
            Some("Display Name")
        );
    }

    #[test]
    fn extract_user_display_name_falls_back_to_username() {
        let payload = serde_json::json!({
            "ok": true,
            "user": {
                "name": "fallback_name",
                "profile": {
                    "display_name": "   ",
                    "real_name": ""
                }
            }
        });

        assert_eq!(
            SlackChannel::extract_user_display_name(&payload).as_deref(),
            Some("fallback_name")
        );
    }

    #[test]
    fn cached_sender_display_name_returns_none_when_expired() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["*".into()]);
        {
            let mut cache = ch.user_display_name_cache.lock().unwrap();
            cache.insert(
                "U123".to_string(),
                CachedSlackDisplayName {
                    display_name: "Expired Name".to_string(),
                    expires_at: Instant::now()
                        .checked_sub(Duration::from_secs(1))
                        .expect("instant should allow subtracting one second in tests"),
                },
            );
        }

        assert_eq!(ch.cached_sender_display_name("U123"), None);
    }

    #[test]
    fn cached_sender_display_name_returns_cached_value_when_valid() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["*".into()]);
        ch.cache_sender_display_name("U123", "Cached Name");

        assert_eq!(
            ch.cached_sender_display_name("U123").as_deref(),
            Some("Cached Name")
        );
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
    fn compose_incoming_content_allows_attachment_only_messages() {
        let composed = SlackChannel::compose_incoming_content(
            String::new(),
            vec!["[IMAGE:data:image/png;base64,aaaa]".to_string()],
        );
        assert_eq!(
            composed.as_deref(),
            Some("[IMAGE:data:image/png;base64,aaaa]")
        );
    }

    #[test]
    fn message_subtype_support_allows_file_share() {
        assert!(SlackChannel::is_supported_message_subtype(None));
        assert!(SlackChannel::is_supported_message_subtype(Some(
            "file_share"
        )));
        assert!(SlackChannel::is_supported_message_subtype(Some(
            "thread_broadcast"
        )));
        assert!(!SlackChannel::is_supported_message_subtype(Some(
            "message_changed"
        )));
        assert!(!SlackChannel::is_supported_message_subtype(Some(
            "channel_join"
        )));
    }

    #[test]
    fn file_text_preview_prefers_preview_field() {
        let file = serde_json::json!({
            "preview": "line 1\nline 2",
            "preview_highlight": "ignored"
        });
        assert_eq!(
            SlackChannel::file_text_preview(&file).as_deref(),
            Some("line 1\nline 2")
        );
    }

    #[test]
    fn is_image_file_detects_mimetype_or_extension() {
        let from_mime = serde_json::json!({"mimetype":"image/png"});
        let from_ext = serde_json::json!({"name":"photo.jpeg"});
        let non_image = serde_json::json!({"name":"notes.txt","mimetype":"text/plain"});
        assert!(SlackChannel::is_image_file(&from_mime));
        assert!(SlackChannel::is_image_file(&from_ext));
        assert!(!SlackChannel::is_image_file(&non_image));
    }

    #[test]
    fn detect_image_mime_rejects_non_image_bytes_despite_image_metadata() {
        let file = serde_json::json!({"mimetype":"image/png","name":"wow.png"});
        let html_bytes = b"<!DOCTYPE html><html><body>login required</body></html>";
        assert_eq!(
            SlackChannel::detect_image_mime(
                Some("image/png"),
                &file,
                html_bytes,
                "https://files.slack.com/files-pri/T1/F2/wow.png"
            ),
            None
        );
    }

    #[test]
    fn detect_image_mime_prefers_magic_bytes_over_misleading_metadata() {
        let file = serde_json::json!({"mimetype":"image/bmp","name":"wow.png"});
        let png_header = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        assert_eq!(
            SlackChannel::detect_image_mime(
                Some("image/bmp"),
                &file,
                &png_header,
                "https://files.slack.com/files-pri/T1/F2/wow.png"
            )
            .as_deref(),
            Some("image/png")
        );
    }

    #[test]
    fn is_probably_text_file_accepts_snippet_mode() {
        let snippet = serde_json::json!({"mode":"snippet"});
        let plain = serde_json::json!({"mimetype":"text/plain"});
        let binary = serde_json::json!({"mimetype":"application/octet-stream","name":"a.bin"});
        assert!(SlackChannel::is_probably_text_file(&snippet));
        assert!(SlackChannel::is_probably_text_file(&plain));
        assert!(!SlackChannel::is_probably_text_file(&binary));
    }

    #[test]
    fn sanitize_attachment_filename_strips_path_traversal() {
        assert_eq!(
            SlackChannel::sanitize_attachment_filename("../../secret.txt").as_deref(),
            Some("secret.txt")
        );
        assert_eq!(
            SlackChannel::sanitize_attachment_filename(r"..\\..\\secret.txt").as_deref(),
            Some("..__..__secret.txt")
        );
        assert!(SlackChannel::sanitize_attachment_filename("..").is_none());
    }

    #[test]
    fn ensure_file_extension_appends_when_missing() {
        assert_eq!(
            SlackChannel::ensure_file_extension("capture", "png"),
            "capture.png"
        );
        assert_eq!(
            SlackChannel::ensure_file_extension("capture.jpeg", "png"),
            "capture.jpeg"
        );
    }

    #[test]
    fn is_allowed_slack_media_hostname_matches_suffixes() {
        assert!(SlackChannel::is_allowed_slack_media_hostname(
            "files.slack.com"
        ));
        assert!(SlackChannel::is_allowed_slack_media_hostname(
            "downloads.slack-edge.com"
        ));
        assert!(SlackChannel::is_allowed_slack_media_hostname(
            "foo.slack-files.com"
        ));
        assert!(!SlackChannel::is_allowed_slack_media_hostname(
            "example.com"
        ));
    }

    #[test]
    fn validate_slack_private_file_url_rejects_invalid_schemes_and_hosts() {
        assert!(
            SlackChannel::validate_slack_private_file_url("https://files.slack.com/f").is_some()
        );
        assert!(
            SlackChannel::validate_slack_private_file_url("http://files.slack.com/f").is_none()
        );
        assert!(SlackChannel::validate_slack_private_file_url("https://example.com/f").is_none());
        assert!(SlackChannel::validate_slack_private_file_url("not a url").is_none());
    }

    #[test]
    fn resolve_https_redirect_target_enforces_https() {
        let base = reqwest::Url::parse("https://files.slack.com/path/file").unwrap();
        let ok = SlackChannel::resolve_https_redirect_target(&base, "/next");
        assert_eq!(
            ok.as_ref().map(|url| url.as_str()),
            Some("https://files.slack.com/next")
        );

        let rejected =
            SlackChannel::resolve_https_redirect_target(&base, "http://files.slack.com/next");
        assert!(rejected.is_none());

        let rejected_host =
            SlackChannel::resolve_https_redirect_target(&base, "https://example.com/next");
        assert!(rejected_host.is_none());
    }

    #[test]
    fn redact_slack_url_hides_query_fragments() {
        let url = reqwest::Url::parse(
            "https://files.slack.com/files-pri/T1/F2/wow.png?token=secret#fragment",
        )
        .unwrap();
        let redacted = SlackChannel::redact_slack_url(&url);
        assert_eq!(redacted, "files.slack.com/.../wow.png");
        assert!(!redacted.contains('?'));
        assert!(!redacted.contains("token="));
        assert!(!redacted.contains('#'));
    }

    #[test]
    fn redact_redirect_location_keeps_only_relative_tail() {
        let redacted =
            SlackChannel::redact_redirect_location("/files-pri/T1/F2/wow.png?token=secret");
        assert_eq!(redacted, "relative/.../wow.png");
        assert!(!redacted.contains("token="));
    }

    #[tokio::test]
    async fn resolve_workspace_attachment_output_path_stays_in_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let output =
            SlackChannel::resolve_workspace_attachment_output_path(workspace.path(), "capture.png")
                .await
                .unwrap();

        let root = tokio::fs::canonicalize(workspace.path()).await.unwrap();
        assert!(output.starts_with(&root));
        assert!(output.to_string_lossy().contains("slack_files"));
    }

    #[tokio::test]
    async fn persist_image_attachment_writes_bytes_without_part_leftovers() {
        let workspace = tempfile::tempdir().unwrap();
        let channel = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![])
            .with_workspace_dir(workspace.path().to_path_buf());
        let file = serde_json::json!({"id":"F1","name":"wow.png"});
        let png_bytes = vec![
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0x00, 0x01, 0x02, 0x03,
        ];

        let output = channel
            .persist_image_attachment(&file, "wow.png", "image/png", &png_bytes)
            .await
            .expect("attachment path");
        let stored = tokio::fs::read(&output).await.expect("stored bytes");
        assert_eq!(stored, png_bytes);

        let save_dir = output.parent().unwrap();
        let mut entries = tokio::fs::read_dir(save_dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.ends_with(".part"),
                "unexpected temp artifact left behind: {name}"
            );
        }
    }

    #[test]
    fn evaluate_health_enforces_socket_mode_probe_when_enabled() {
        assert!(!SlackChannel::evaluate_health(false, false, true));
        assert!(!SlackChannel::evaluate_health(false, true, true));
        assert!(SlackChannel::evaluate_health(true, false, false));
        assert!(SlackChannel::evaluate_health(true, false, true));
        assert!(!SlackChannel::evaluate_health(true, true, false));
        assert!(SlackChannel::evaluate_health(true, true, true));
    }

    #[test]
    fn slack_api_call_succeeded_requires_ok_true_in_body() {
        assert!(!SlackChannel::slack_api_call_succeeded(
            reqwest::StatusCode::OK,
            r#"{"ok":false,"error":"invalid_auth"}"#
        ));
    }

    #[test]
    fn slack_api_call_succeeded_accepts_ok_true() {
        assert!(SlackChannel::slack_api_call_succeeded(
            reqwest::StatusCode::OK,
            r#"{"ok":true}"#
        ));
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            None,
            None,
            vec![],
            vec!["U111".into(), "U222".into()],
        );
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("U222"));
        assert!(!ch.is_user_allowed("U333"));
    }

    #[test]
    fn allowlist_exact_match_not_substring() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["U111".into()]);
        assert!(!ch.is_user_allowed("U1111"));
        assert!(!ch.is_user_allowed("U11"));
    }

    #[test]
    fn allowlist_empty_user_id() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["U111".into()]);
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec!["U111".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(!ch.is_user_allowed("u111"));
    }

    #[test]
    fn allowlist_wildcard_and_specific() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            None,
            None,
            vec![],
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

    // ── Thread reply handling ────────────────────────────────────

    #[test]
    fn extract_active_threads_finds_thread_parents_with_replies() {
        let messages = vec![
            serde_json::json!({
                "ts": "100.000",
                "thread_ts": "100.000",
                "reply_count": 3,
                "latest_reply": "103.000"
            }),
            serde_json::json!({
                "ts": "200.000",
                "text": "no thread"
            }),
            serde_json::json!({
                "ts": "300.000",
                "thread_ts": "300.000",
                "reply_count": 0
            }),
        ];

        let threads = SlackChannel::extract_active_threads(&messages);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].0, "100.000");
        assert_eq!(threads[0].1, "103.000");
    }

    #[test]
    fn extract_active_threads_ignores_reply_messages() {
        // A reply message has ts != thread_ts; it should not be treated as a thread parent.
        let messages = vec![serde_json::json!({
            "ts": "101.000",
            "thread_ts": "100.000",
            "text": "reply in thread"
        })];

        let threads = SlackChannel::extract_active_threads(&messages);
        assert!(threads.is_empty());
    }

    #[test]
    fn extract_active_threads_uses_thread_ts_as_fallback_latest_reply() {
        let messages = vec![serde_json::json!({
            "ts": "100.000",
            "thread_ts": "100.000",
            "reply_count": 1
        })];

        let threads = SlackChannel::extract_active_threads(&messages);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].1, "100.000");
    }

    #[test]
    fn evict_stale_threads_removes_expired_entries() {
        let mut threads: HashMap<String, (String, String, Instant)> = HashMap::new();
        let old = Instant::now()
            .checked_sub(Duration::from_secs(SLACK_POLL_THREAD_EXPIRE_SECS + 1))
            .unwrap();
        threads.insert(
            "old.thread".to_string(),
            ("C1".to_string(), "old.reply".to_string(), old),
        );
        threads.insert(
            "new.thread".to_string(),
            ("C1".to_string(), "new.reply".to_string(), Instant::now()),
        );

        SlackChannel::evict_stale_threads(&mut threads, Instant::now());
        assert_eq!(threads.len(), 1);
        assert!(threads.contains_key("new.thread"));
    }

    #[test]
    fn evict_stale_threads_trims_excess_by_oldest_key() {
        let mut threads: HashMap<String, (String, String, Instant)> = HashMap::new();
        let now = Instant::now();
        for i in 0..(SLACK_POLL_ACTIVE_THREAD_MAX + 5) {
            threads.insert(
                format!("{i:06}.000"),
                ("C1".to_string(), format!("{i:06}.001"), now),
            );
        }

        SlackChannel::evict_stale_threads(&mut threads, now);
        assert_eq!(threads.len(), SLACK_POLL_ACTIVE_THREAD_MAX);
    }

    #[test]
    fn is_supported_message_subtype_rejects_message_replied() {
        // message_replied is a parent-level notification, not an actual reply.
        assert!(!SlackChannel::is_supported_message_subtype(Some(
            "message_replied"
        )));
    }

    #[test]
    fn extract_slack_ts_from_standard_message_id() {
        assert_eq!(
            extract_slack_ts("slack_C1234567890_1234567890.123456"),
            "1234567890.123456"
        );
    }

    #[test]
    fn extract_slack_ts_from_raw_ts_passthrough() {
        assert_eq!(extract_slack_ts("1234567890.123456"), "1234567890.123456");
    }

    #[test]
    fn extract_slack_ts_from_unprefixed_id() {
        assert_eq!(extract_slack_ts("unknown_format"), "unknown_format");
    }

    #[test]
    fn unicode_emoji_maps_to_slack_eyes() {
        assert_eq!(unicode_emoji_to_slack_name("\u{1F440}"), "eyes");
    }

    #[test]
    fn unicode_emoji_maps_to_slack_check_mark() {
        assert_eq!(unicode_emoji_to_slack_name("\u{2705}"), "white_check_mark");
    }

    #[test]
    fn unicode_emoji_maps_to_slack_warning() {
        assert_eq!(unicode_emoji_to_slack_name("\u{26A0}\u{FE0F}"), "warning");
        assert_eq!(unicode_emoji_to_slack_name("\u{26A0}"), "warning");
    }

    #[test]
    fn unicode_emoji_colon_wrapped_passthrough() {
        assert_eq!(
            unicode_emoji_to_slack_name(":custom_emoji:"),
            "custom_emoji"
        );
    }

    #[test]
    fn inbound_thread_ts_on_thread_reply_uses_thread_ts() {
        let reply = serde_json::json!({
            "ts": "200.000",
            "thread_ts": "100.000",
            "text": "a thread reply"
        });
        let thread_ts = SlackChannel::inbound_thread_ts(&reply, "200.000");
        assert_eq!(thread_ts.as_deref(), Some("100.000"));
    }

    #[test]
    fn inbound_thread_ts_genuine_only_returns_none_for_top_level() {
        // Top-level messages don't have thread_ts in Slack's API.
        let msg = serde_json::json!({
            "ts": "100.000",
            "text": "hello"
        });
        assert_eq!(SlackChannel::inbound_thread_ts_genuine_only(&msg), None);
    }

    #[test]
    fn inbound_thread_ts_genuine_only_returns_thread_ts_for_replies() {
        // Thread replies have thread_ts pointing to the parent message.
        let reply = serde_json::json!({
            "ts": "200.000",
            "thread_ts": "100.000",
            "text": "a reply"
        });
        assert_eq!(
            SlackChannel::inbound_thread_ts_genuine_only(&reply).as_deref(),
            Some("100.000")
        );
    }

    #[test]
    fn session_key_stable_without_thread_replies() {
        // When thread_replies=false, top-level messages from the same user should
        // produce the same conversation_history_key (thread_ts=None).
        use crate::channels::traits::ChannelMessage;

        let make_msg = |ts: &str| ChannelMessage {
            id: format!("slack_C123_{ts}"),
            sender: "U_alice".into(),
            reply_target: "C123".into(),
            content: "text".into(),
            channel: "slack".into(),
            timestamp: 0,
            thread_ts: None, // thread_replies=false → no fallback to ts
            interruption_scope_id: None,
            attachments: vec![],
        };

        let msg1 = make_msg("100.000");
        let msg2 = make_msg("200.000");

        let key1 = super::super::conversation_history_key(&msg1);
        let key2 = super::super::conversation_history_key(&msg2);
        assert_eq!(key1, key2, "session key should be stable across messages");
    }

    #[test]
    fn session_key_varies_with_thread_replies() {
        // When thread_replies=true, top-level messages get thread_ts=Some(ts),
        // giving each its own session key (thread isolation).
        use crate::channels::traits::ChannelMessage;

        let make_msg = |ts: &str| ChannelMessage {
            id: format!("slack_C123_{ts}"),
            sender: "U_alice".into(),
            reply_target: "C123".into(),
            content: "text".into(),
            channel: "slack".into(),
            timestamp: 0,
            thread_ts: Some(ts.to_string()), // thread_replies=true → ts as thread_ts
            interruption_scope_id: None,
            attachments: vec![],
        };

        let msg1 = make_msg("100.000");
        let msg2 = make_msg("200.000");

        let key1 = super::super::conversation_history_key(&msg1);
        let key2 = super::super::conversation_history_key(&msg2);
        assert_ne!(key1, key2, "session key should differ per thread");
    }

    #[test]
    fn slack_send_uses_markdown_blocks() {
        let msg = SendMessage::new("**bold** and _italic_", "C123");
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![]);

        // Build the same JSON body that send() would construct.
        let mut body = serde_json::json!({
            "channel": msg.recipient,
            "text": msg.content
        });
        if msg.content.len() <= SLACK_MARKDOWN_BLOCK_MAX_CHARS {
            body["blocks"] = serde_json::json!([{
                "type": "markdown",
                "text": msg.content
            }]);
        }

        // Verify blocks are present with correct structure.
        let blocks = body["blocks"]
            .as_array()
            .expect("blocks should be an array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "markdown");
        assert_eq!(blocks[0]["text"], msg.content);
        // text field kept as plaintext fallback.
        assert_eq!(body["text"], msg.content);
        // Suppress unused variable warning.
        let _ = ch.name();
    }

    #[test]
    fn slack_send_skips_markdown_blocks_for_long_content() {
        let long_content = "x".repeat(SLACK_MARKDOWN_BLOCK_MAX_CHARS + 1);
        let msg = SendMessage::new(long_content.clone(), "C123");

        let mut body = serde_json::json!({
            "channel": msg.recipient,
            "text": msg.content
        });
        if msg.content.len() <= SLACK_MARKDOWN_BLOCK_MAX_CHARS {
            body["blocks"] = serde_json::json!([{
                "type": "markdown",
                "text": msg.content
            }]);
        }

        assert!(
            body.get("blocks").is_none(),
            "blocks should not be set for oversized content"
        );
    }

    #[tokio::test]
    async fn start_typing_requires_thread_context() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![]);
        // No thread_ts tracked for "C999" — start_typing should be a no-op (Ok).
        let result = ch.start_typing("C999").await;
        assert!(
            result.is_ok(),
            "start_typing should succeed as no-op without thread context"
        );
    }

    #[test]
    fn assistant_thread_tracking() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, None, vec![], vec![]);

        // Initially empty.
        {
            let map = ch.active_assistant_thread.lock().unwrap();
            assert!(map.is_empty());
        }

        // Simulate storing a thread_ts (as listen_socket_mode would).
        {
            let mut map = ch.active_assistant_thread.lock().unwrap();
            map.insert("C123".to_string(), "1741234567.000100".to_string());
        }

        // Verify retrieval.
        {
            let map = ch.active_assistant_thread.lock().unwrap();
            assert_eq!(map.get("C123"), Some(&"1741234567.000100".to_string()),);
            assert_eq!(map.get("C999"), None);
        }
    }
}
