use anyhow::{Result, bail};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const MAX_MATTERMOST_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

/// Mattermost channel — connects via WebSocket for real-time messages.
/// When `channel_id` is set (and not `"*"`), only events from that channel are processed.
/// When `channel_id` is omitted or `"*"`, all channels and DMs are received.
pub struct MattermostChannel {
    base_url: String, // e.g., https://mm.example.com
    bot_token: String,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
    /// When true (default), replies thread on the original post's root_id.
    /// When false, replies go to the channel root.
    thread_replies: bool,
    /// When true, only respond to messages that @-mention the bot.
    mention_only: bool,
    /// Handle for the background typing-indicator loop (aborted on stop_typing).
    typing_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
}

impl MattermostChannel {
    pub fn new(
        base_url: String,
        bot_token: String,
        channel_id: Option<String>,
        allowed_users: Vec<String>,
        thread_replies: bool,
        mention_only: bool,
    ) -> Self {
        // Ensure base_url doesn't have a trailing slash for consistent path joining
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            base_url,
            bot_token,
            channel_id,
            allowed_users,
            thread_replies,
            mention_only,
            typing_handle: Mutex::new(None),
            proxy_url: None,
            transcription: None,
            transcription_manager: None,
        }
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(Arc::new(m));
                self.transcription = Some(config);
            }
            Err(e) => {
                tracing::warn!(
                    "transcription manager init failed, voice transcription disabled: {e}"
                );
            }
        }
        self
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.mattermost",
            self.proxy_url.as_deref(),
        )
    }

    /// Check if a user ID is in the allowlist.
    /// Empty list means deny everyone. "*" means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Get the bot's own user ID and username so we can ignore our own messages
    /// and detect @-mentions by username.
    async fn get_bot_identity(&self) -> (String, String) {
        let resp: Option<serde_json::Value> = async {
            self.http_client()
                .get(format!("{}/api/v4/users/me", self.base_url))
                .bearer_auth(&self.bot_token)
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()
        }
        .await;

        let id = resp
            .as_ref()
            .and_then(|v| v.get("id"))
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string();
        let username = resp
            .as_ref()
            .and_then(|v| v.get("username"))
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string();
        (id, username)
    }

    async fn try_transcribe_audio_attachment(&self, post: &serde_json::Value) -> Option<String> {
        let config = self.transcription.as_ref()?;
        let manager = self.transcription_manager.as_deref()?;

        let files = post
            .get("metadata")
            .and_then(|m| m.get("files"))
            .and_then(|f| f.as_array())?;

        let audio_file = files.iter().find(|f| is_audio_file(f))?;

        if let Some(duration_ms) = audio_file.get("duration").and_then(|d| d.as_u64()) {
            let duration_secs = duration_ms / 1000;
            if duration_secs > config.max_duration_secs {
                tracing::debug!(
                    duration_secs,
                    max = config.max_duration_secs,
                    "Mattermost audio attachment exceeds max duration, skipping"
                );
                return None;
            }
        }

        let file_id = audio_file.get("id").and_then(|i| i.as_str())?;
        let file_name = audio_file
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("audio");

        let response = match self
            .http_client()
            .get(format!("{}/api/v4/files/{}", self.base_url, file_id))
            .bearer_auth(&self.bot_token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Mattermost: audio download failed for {file_id}: {e}");
                return None;
            }
        };

        if !response.status().is_success() {
            tracing::warn!(
                "Mattermost: audio download returned {}: {file_id}",
                response.status()
            );
            return None;
        }

        if let Some(content_length) = response.content_length()
            && content_length > MAX_MATTERMOST_AUDIO_BYTES
        {
            tracing::warn!("Mattermost: audio file too large ({content_length} bytes): {file_id}");
            return None;
        }

        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Mattermost: failed to read audio bytes for {file_id}: {e}");
                return None;
            }
        };

        match manager.transcribe(&bytes, file_name).await {
            Ok(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    tracing::info!("Mattermost: transcription returned empty text, skipping");
                    None
                } else {
                    Some(format!("[Voice] {trimmed}"))
                }
            }
            Err(e) => {
                tracing::warn!("Mattermost audio transcription failed: {e}");
                None
            }
        }
    }

    /// Build the WebSocket URL from the base HTTP URL.
    fn ws_url(&self) -> String {
        let base = self
            .base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        format!("{base}/api/v4/websocket")
    }

    /// Whether a given channel_id passes the configured filter.
    fn channel_matches_filter(&self, event_channel_id: &str) -> bool {
        match &self.channel_id {
            Some(id) if id != "*" => id == event_channel_id,
            _ => true,
        }
    }

    /// WebSocket event loop. Returns `Ok(())` when the connection closes
    /// (the channel supervisor reconnects by calling `listen()` again).
    async fn listen_websocket(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let (bot_user_id, bot_username) = self.get_bot_identity().await;
        let filter_desc = match &self.channel_id {
            Some(id) if id != "*" => format!("channel {id}"),
            _ => "all channels/DMs".to_string(),
        };

        tracing::info!("Mattermost: connecting to WebSocket ({filter_desc})...");

        let ws_url = self.ws_url();
        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Authenticate via authentication_challenge handshake.
        let auth_msg = serde_json::json!({
            "seq": 1,
            "action": "authentication_challenge",
            "data": { "token": self.bot_token }
        });
        write.send(WsMsg::Text(auth_msg.to_string().into())).await?;

        tracing::info!("Mattermost: connected and authenticated");

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    bail!("Mattermost: websocket read error: {e}");
                }
            };

            let text = match &msg {
                WsMsg::Text(t) => t.as_ref(),
                WsMsg::Close(_) => {
                    tracing::info!("Mattermost: WS closed by server");
                    break;
                }
                _ => continue,
            };

            let event: serde_json::Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if event.get("event").and_then(|e| e.as_str()) != Some("posted") {
                continue;
            }

            let data = match event.get("data") {
                Some(d) => d,
                None => continue,
            };

            let event_channel_id = event
                .get("broadcast")
                .and_then(|b| b.get("channel_id"))
                .and_then(|c| c.as_str())
                .unwrap_or("");

            if !event_channel_id.is_empty() && !self.channel_matches_filter(event_channel_id) {
                continue;
            }

            // Parse the double-encoded post JSON
            let post_str = match data.get("post").and_then(|p| p.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let post: serde_json::Value = match serde_json::from_str(post_str) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let channel_id = post
                .get("channel_id")
                .and_then(|c| c.as_str())
                .unwrap_or(event_channel_id);

            if channel_id.is_empty() {
                continue;
            }

            let channel_type = data
                .get("channel_type")
                .and_then(|c| c.as_str())
                .unwrap_or("");

            let effective_text = if post
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .trim()
                .is_empty()
                && post_has_audio_attachment(&post)
            {
                self.try_transcribe_audio_attachment(&post).await
            } else {
                None
            };

            if let Some(channel_msg) = self.parse_mattermost_post(
                &post,
                &bot_user_id,
                &bot_username,
                0,
                channel_id,
                channel_type,
                effective_text.as_deref(),
            ) && tx.send(channel_msg).await.is_err()
            {
                return Ok(());
            }
        }

        Ok(())
    }

    /// REST polling fallback — listens on a single channel via periodic GET requests.
    #[allow(dead_code)]
    async fn listen_polling(
        &self,
        channel_id: &str,
        tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let (bot_user_id, bot_username) = self.get_bot_identity().await;
        #[allow(clippy::cast_possible_truncation)]
        let mut last_create_at = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()) as i64;

        tracing::info!("Mattermost channel polling on {}...", channel_id);

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            let resp = match self
                .http_client()
                .get(format!(
                    "{}/api/v4/channels/{}/posts",
                    self.base_url, channel_id
                ))
                .bearer_auth(&self.bot_token)
                .query(&[("since", last_create_at.to_string())])
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Mattermost poll error: {e}");
                    continue;
                }
            };

            let data: serde_json::Value = match resp.json().await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("Mattermost parse error: {e}");
                    continue;
                }
            };

            if let Some(posts) = data.get("posts").and_then(|p| p.as_object()) {
                let mut post_list: Vec<_> = posts.values().collect();
                post_list.sort_by_key(|p| p.get("create_at").and_then(|c| c.as_i64()).unwrap_or(0));

                let last_create_at_before_this_batch = last_create_at;
                for post in post_list {
                    let create_at = post
                        .get("create_at")
                        .and_then(|c| c.as_i64())
                        .unwrap_or(last_create_at);
                    last_create_at = last_create_at.max(create_at);

                    let effective_text = if post
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                        && post_has_audio_attachment(post)
                    {
                        self.try_transcribe_audio_attachment(post).await
                    } else {
                        None
                    };

                    if let Some(channel_msg) = self.parse_mattermost_post(
                        post,
                        &bot_user_id,
                        &bot_username,
                        last_create_at_before_this_batch,
                        channel_id,
                        "",
                        effective_text.as_deref(),
                    ) && tx.send(channel_msg).await.is_err()
                    {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Channel for MattermostChannel {
    fn name(&self) -> &str {
        "mattermost"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Mattermost supports threading via 'root_id'.
        // We pack 'channel_id:root_id' into recipient if it's a thread.
        let (channel_id, root_id) = if let Some((c, r)) = message.recipient.split_once(':') {
            (c, Some(r))
        } else {
            (message.recipient.as_str(), None)
        };

        let mut body_map = serde_json::json!({
            "channel_id": channel_id,
            "message": message.content
        });

        if let Some(root) = root_id {
            body_map.as_object_mut().unwrap().insert(
                "root_id".to_string(),
                serde_json::Value::String(root.to_string()),
            );
        }

        let resp = self
            .http_client()
            .post(format!("{}/api/v4/posts", self.base_url))
            .bearer_auth(&self.bot_token)
            .json(&body_map)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("Mattermost post failed ({status}): {body}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        self.listen_websocket(tx).await
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get(format!("{}/api/v4/users/me", self.base_url))
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        // Cancel any existing typing loop before starting a new one.
        self.stop_typing(recipient).await?;

        let client = self.http_client();
        let token = self.bot_token.clone();
        let base_url = self.base_url.clone();

        // recipient is "channel_id" or "channel_id:root_id"
        let (channel_id, parent_id) = match recipient.split_once(':') {
            Some((channel, parent)) => (channel.to_string(), Some(parent.to_string())),
            None => (recipient.to_string(), None),
        };

        let handle = tokio::spawn(async move {
            let url = format!("{base_url}/api/v4/users/me/typing");
            loop {
                let mut body = serde_json::json!({ "channel_id": channel_id });
                if let Some(ref pid) = parent_id {
                    body.as_object_mut()
                        .unwrap()
                        .insert("parent_id".to_string(), serde_json::json!(pid));
                }

                if let Ok(r) = client
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&body)
                    .send()
                    .await
                    && !r.status().is_success()
                {
                    tracing::debug!(status = %r.status(), "Mattermost typing indicator failed");
                }

                // Mattermost typing events expire after ~6s; re-fire every 4s.
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
        });

        let mut guard = self.typing_handle.lock();
        *guard = Some(handle);

        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        let mut guard = self.typing_handle.lock();
        if let Some(handle) = guard.take() {
            handle.abort();
        }
        Ok(())
    }
}

impl MattermostChannel {
    fn parse_mattermost_post(
        &self,
        post: &serde_json::Value,
        bot_user_id: &str,
        bot_username: &str,
        last_create_at: i64,
        channel_id: &str,
        channel_type: &str,
        injected_text: Option<&str>,
    ) -> Option<ChannelMessage> {
        let id = post.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let user_id = post.get("user_id").and_then(|u| u.as_str()).unwrap_or("");
        let text = post.get("message").and_then(|m| m.as_str()).unwrap_or("");
        let create_at = post.get("create_at").and_then(|c| c.as_i64()).unwrap_or(0);
        let root_id = post.get("root_id").and_then(|r| r.as_str()).unwrap_or("");

        if user_id == bot_user_id || create_at <= last_create_at {
            return None;
        }

        let effective_text = if text.is_empty() {
            injected_text?
        } else {
            text
        };

        if !self.is_user_allowed(user_id) {
            tracing::warn!("Mattermost: ignoring message from unauthorized user: {user_id}");
            return None;
        }

        // mention_only filtering: skip messages that don't @-mention the bot.
        // Exceptions (always respond without mention):
        //   - DMs (channel_type "D"), matching Lark/WhatsApp behavior.
        //   - Thread replies (root_id set), matching Slack behavior — once the
        //     bot is in a thread, all replies are processed without @-mention.
        let is_dm = channel_type == "D";
        let in_thread = !root_id.is_empty();
        let content = if self.mention_only && !is_dm && !in_thread {
            let normalized =
                normalize_mattermost_content(effective_text, bot_user_id, bot_username, post);
            normalized?
        } else {
            effective_text.to_string()
        };

        // Reply routing depends on thread_replies config:
        //   - Existing thread (root_id set): always stay in the thread.
        //   - Top-level post + thread_replies=true: thread on the original post.
        //   - Top-level post + thread_replies=false: reply at channel level.
        let reply_target = if !root_id.is_empty() {
            format!("{}:{}", channel_id, root_id)
        } else if self.thread_replies {
            format!("{}:{}", channel_id, id)
        } else {
            channel_id.to_string()
        };

        Some(ChannelMessage {
            id: format!("mattermost_{id}"),
            sender: user_id.to_string(),
            reply_target,
            content,
            channel: "mattermost".to_string(),
            #[allow(clippy::cast_sign_loss)]
            timestamp: (create_at / 1000) as u64,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        })
    }
}

fn post_has_audio_attachment(post: &serde_json::Value) -> bool {
    let files = post
        .get("metadata")
        .and_then(|m| m.get("files"))
        .and_then(|f| f.as_array());
    let Some(files) = files else { return false };
    files.iter().any(is_audio_file)
}

fn is_audio_file(file: &serde_json::Value) -> bool {
    let mime = file.get("mime_type").and_then(|m| m.as_str()).unwrap_or("");
    if mime.starts_with("audio/") {
        return true;
    }
    let ext = file.get("extension").and_then(|e| e.as_str()).unwrap_or("");
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "ogg" | "mp3" | "m4a" | "wav" | "opus" | "flac"
    )
}

/// Check whether a Mattermost post contains an @-mention of the bot.
///
/// Checks two sources:
/// 1. Text-based: looks for `@bot_username` in the message body (case-insensitive).
/// 2. Metadata-based: checks the post's `metadata.mentions` array for the bot user ID.
#[cfg(test)]
fn contains_bot_mention_mm(
    text: &str,
    bot_user_id: &str,
    bot_username: &str,
    post: &serde_json::Value,
) -> bool {
    // 1. Text-based: @username (case-insensitive, word-boundary aware)
    if !find_bot_mention_spans(text, bot_username).is_empty() {
        return true;
    }

    // 2. Metadata-based: Mattermost may include a "metadata.mentions" array of user IDs.
    if !bot_user_id.is_empty()
        && let Some(mentions) = post
            .get("metadata")
            .and_then(|m| m.get("mentions"))
            .and_then(|m| m.as_array())
        && mentions.iter().any(|m| m.as_str() == Some(bot_user_id))
    {
        return true;
    }

    false
}

fn is_mattermost_username_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

fn find_bot_mention_spans(text: &str, bot_username: &str) -> Vec<(usize, usize)> {
    if bot_username.is_empty() {
        return Vec::new();
    }

    let mention = format!("@{}", bot_username.to_ascii_lowercase());
    let mention_len = mention.len();
    if mention_len == 0 {
        return Vec::new();
    }

    let mention_bytes = mention.as_bytes();
    let text_bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;

    while index + mention_len <= text_bytes.len() {
        let is_match = text_bytes[index] == b'@'
            && text_bytes[index..index + mention_len]
                .iter()
                .zip(mention_bytes.iter())
                .all(|(left, right)| left.eq_ignore_ascii_case(right));

        if is_match {
            let end = index + mention_len;
            let at_boundary = text[end..]
                .chars()
                .next()
                .is_none_or(|next| !is_mattermost_username_char(next));
            if at_boundary {
                spans.push((index, end));
                index = end;
                continue;
            }
        }

        let step = text[index..].chars().next().map_or(1, char::len_utf8);
        index += step;
    }

    spans
}

/// Normalize incoming Mattermost content when `mention_only` is enabled.
///
/// Returns `None` if the message doesn't mention the bot.
/// Returns `Some(cleaned)` with the @-mention stripped and text trimmed.
fn normalize_mattermost_content(
    text: &str,
    bot_user_id: &str,
    bot_username: &str,
    post: &serde_json::Value,
) -> Option<String> {
    let mention_spans = find_bot_mention_spans(text, bot_username);
    let metadata_mentions_bot = !bot_user_id.is_empty()
        && post
            .get("metadata")
            .and_then(|m| m.get("mentions"))
            .and_then(|m| m.as_array())
            .is_some_and(|mentions| mentions.iter().any(|m| m.as_str() == Some(bot_user_id)));

    if mention_spans.is_empty() && !metadata_mentions_bot {
        return None;
    }

    let mut cleaned = text.to_string();
    if !mention_spans.is_empty() {
        let mut result = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end) in mention_spans {
            result.push_str(&text[cursor..start]);
            result.push(' ');
            cursor = end;
        }
        result.push_str(&text[cursor..]);
        cleaned = result;
    }

    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return None;
    }

    Some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Helper: create a channel with mention_only=false (legacy behavior).
    fn make_channel(allowed: Vec<String>, thread_replies: bool) -> MattermostChannel {
        MattermostChannel::new(
            "url".into(),
            "token".into(),
            None,
            allowed,
            thread_replies,
            false,
        )
    }

    // Helper: create a channel with mention_only=true.
    fn make_mention_only_channel() -> MattermostChannel {
        MattermostChannel::new(
            "url".into(),
            "token".into(),
            None,
            vec!["*".into()],
            true,
            true,
        )
    }

    #[test]
    fn mattermost_url_trimming() {
        let ch = MattermostChannel::new(
            "https://mm.example.com/".into(),
            "token".into(),
            None,
            vec![],
            false,
            false,
        );
        assert_eq!(ch.base_url, "https://mm.example.com");
    }

    #[test]
    fn mattermost_allowlist_wildcard() {
        let ch = make_channel(vec!["*".into()], false);
        assert!(ch.is_user_allowed("any-id"));
    }

    #[test]
    fn mattermost_parse_post_basic() {
        let ch = make_channel(vec!["*".into()], true);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.sender, "user456");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.reply_target, "chan789:post123"); // Default threaded reply
    }

    #[test]
    fn mattermost_parse_post_thread_replies_enabled() {
        let ch = make_channel(vec!["*".into()], true);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789:post123"); // Threaded reply
    }

    #[test]
    fn mattermost_parse_post_thread() {
        let ch = make_channel(vec!["*".into()], false);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "reply",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "root789"
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789:root789"); // Stays in the thread
    }

    #[test]
    fn mattermost_parse_post_ignore_self() {
        let ch = make_channel(vec!["*".into()], false);
        let post = json!({
            "id": "post123",
            "user_id": "bot123",
            "message": "my own message",
            "create_at": 1_600_000_000_000_i64
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "botname",
            1_500_000_000_000_i64,
            "chan789",
            "O",
            None,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mattermost_parse_post_ignore_old() {
        let ch = make_channel(vec!["*".into()], false);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "old message",
            "create_at": 1_400_000_000_000_i64
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "botname",
            1_500_000_000_000_i64,
            "chan789",
            "O",
            None,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mattermost_parse_post_no_thread_when_disabled() {
        let ch = make_channel(vec!["*".into()], false);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789"); // No thread suffix
    }

    #[test]
    fn mattermost_existing_thread_always_threads() {
        // Even with thread_replies=false, replies to existing threads stay in the thread
        let ch = make_channel(vec!["*".into()], false);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "reply in thread",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "root789"
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789:root789"); // Stays in existing thread
    }

    // ── mention_only tests ────────────────────────────────────────

    #[test]
    fn mention_only_skips_message_without_mention() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hello everyone",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "mybot",
            1_500_000_000_000_i64,
            "chan1",
            "O",
            None,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mention_only_accepts_message_with_at_mention() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@mybot what is the weather?",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "what is the weather?");
    }

    #[test]
    fn mention_only_strips_mention_and_trims() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "  @mybot  run status  ",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "run status");
    }

    #[test]
    fn mention_only_rejects_empty_after_stripping() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@mybot",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "mybot",
            1_500_000_000_000_i64,
            "chan1",
            "O",
            None,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mention_only_case_insensitive() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@MyBot hello",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "hello");
    }

    #[test]
    fn mention_only_detects_metadata_mentions() {
        // Even without @username in text, metadata.mentions should trigger.
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hey check this out",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "",
            "metadata": {
                "mentions": ["bot123"]
            }
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                "O",
                None,
            )
            .unwrap();
        // Content is preserved as-is since no @username was in the text to strip.
        assert_eq!(msg.content, "hey check this out");
    }

    #[test]
    fn mention_only_word_boundary_prevents_partial_match() {
        let ch = make_mention_only_channel();
        // "@mybotextended" should NOT match "@mybot" because it extends the username.
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@mybotextended hello",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "mybot",
            1_500_000_000_000_i64,
            "chan1",
            "O",
            None,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mention_only_mention_in_middle_of_text() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hey @mybot how are you?",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "hey   how are you?");
    }

    #[test]
    fn mention_only_disabled_passes_all_messages() {
        // With mention_only=false (default), messages pass through unfiltered.
        let ch = make_channel(vec!["*".into()], true);
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "no mention here",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "no mention here");
    }

    // ── contains_bot_mention_mm unit tests ────────────────────────

    #[test]
    fn contains_mention_text_at_end() {
        let post = json!({});
        assert!(contains_bot_mention_mm(
            "hello @mybot",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn contains_mention_text_at_start() {
        let post = json!({});
        assert!(contains_bot_mention_mm(
            "@mybot hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn contains_mention_text_alone() {
        let post = json!({});
        assert!(contains_bot_mention_mm("@mybot", "bot123", "mybot", &post));
    }

    #[test]
    fn no_mention_different_username() {
        let post = json!({});
        assert!(!contains_bot_mention_mm(
            "@otherbot hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn no_mention_partial_username() {
        let post = json!({});
        // "mybot" is a prefix of "mybotx" — should NOT match
        assert!(!contains_bot_mention_mm(
            "@mybotx hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn mention_detects_later_valid_mention_after_partial_prefix() {
        let post = json!({});
        assert!(contains_bot_mention_mm(
            "@mybotx ignore this, but @mybot handle this",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn mention_followed_by_punctuation() {
        let post = json!({});
        // "@mybot," — comma is not alphanumeric/underscore/dash/dot, so it's a boundary
        assert!(contains_bot_mention_mm(
            "@mybot, hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn mention_via_metadata_only() {
        let post = json!({
            "metadata": { "mentions": ["bot123"] }
        });
        assert!(contains_bot_mention_mm(
            "no at mention",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn no_mention_empty_username_no_metadata() {
        let post = json!({});
        assert!(!contains_bot_mention_mm("hello world", "bot123", "", &post));
    }

    // ── normalize_mattermost_content unit tests ───────────────────

    #[test]
    fn normalize_strips_and_trims() {
        let post = json!({});
        let result = normalize_mattermost_content("  @mybot  do stuff  ", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("do stuff"));
    }

    #[test]
    fn normalize_returns_none_for_no_mention() {
        let post = json!({});
        let result = normalize_mattermost_content("hello world", "bot123", "mybot", &post);
        assert!(result.is_none());
    }

    #[test]
    fn normalize_returns_none_when_only_mention() {
        let post = json!({});
        let result = normalize_mattermost_content("@mybot", "bot123", "mybot", &post);
        assert!(result.is_none());
    }

    #[test]
    fn normalize_preserves_text_for_metadata_mention() {
        let post = json!({
            "metadata": { "mentions": ["bot123"] }
        });
        let result = normalize_mattermost_content("check this out", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("check this out"));
    }

    #[test]
    fn normalize_strips_multiple_mentions() {
        let post = json!({});
        let result =
            normalize_mattermost_content("@mybot hello @mybot world", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("hello   world"));
    }

    #[test]
    fn normalize_keeps_partial_username_mentions() {
        let post = json!({});
        let result =
            normalize_mattermost_content("@mybot hello @mybotx world", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("hello @mybotx world"));
    }

    // ── Transcription tests ───────────────────────────────────────

    #[test]
    fn mattermost_manager_none_when_transcription_not_configured() {
        let ch = make_channel(vec!["*".into()], false);
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn mattermost_manager_some_when_valid_config() {
        let ch = make_channel(vec!["*".into()], false).with_transcription(
            zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                default_provider: "groq".to_string(),
                api_key: Some("test_key".to_string()),
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_duration_secs: 600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: None,
                transcribe_non_ptt_audio: false,
            },
        );
        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    fn mattermost_manager_none_and_warn_on_init_failure() {
        let ch = make_channel(vec!["*".into()], false).with_transcription(
            zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                default_provider: "groq".to_string(),
                api_key: Some(String::new()),
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_duration_secs: 600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: None,
                transcribe_non_ptt_audio: false,
            },
        );
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn mattermost_post_has_audio_attachment_true_for_audio_mime() {
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "audio/ogg",
                        "name": "voice.ogg"
                    }
                ]
            }
        });
        assert!(post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_post_has_audio_attachment_true_for_audio_ext() {
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "application/octet-stream",
                        "extension": "ogg"
                    }
                ]
            }
        });
        assert!(post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_post_has_audio_attachment_false_for_image() {
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "image/png",
                        "name": "screenshot.png"
                    }
                ]
            }
        });
        assert!(!post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_post_has_audio_attachment_false_when_no_files() {
        let post = json!({
            "metadata": {}
        });
        assert!(!post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_parse_post_uses_injected_text() {
        let ch = make_channel(vec!["*".into()], true);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                "O",
                Some("transcript text"),
            )
            .unwrap();
        assert_eq!(msg.content, "transcript text");
    }

    #[test]
    fn mattermost_parse_post_rejects_empty_message_without_injected() {
        let ch = make_channel(vec!["*".into()], true);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "botname",
            1_500_000_000_000_i64,
            "chan789",
            "O",
            None,
        );
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn mattermost_transcribe_skips_when_manager_none() {
        let ch = make_channel(vec!["*".into()], false);
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "audio/ogg",
                        "name": "voice.ogg"
                    }
                ]
            }
        });
        let result = ch.try_transcribe_audio_attachment(&post).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn mattermost_transcribe_skips_over_duration_limit() {
        let ch = make_channel(vec!["*".into()], false).with_transcription(
            zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                default_provider: "groq".to_string(),
                api_key: Some("test_key".to_string()),
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_duration_secs: 3600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: None,
                transcribe_non_ptt_audio: false,
            },
        );

        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "audio/ogg",
                        "name": "voice.ogg",
                        "duration": 7_200_000_u64
                    }
                ]
            }
        });

        let result = ch.try_transcribe_audio_attachment(&post).await;
        assert!(result.is_none());
    }

    #[cfg(test)]
    mod http_tests {
        use super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn mattermost_audio_routes_through_local_whisper() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/api/v4/files/file1"))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b"audio bytes"))
                .mount(&mock_server)
                .await;

            Mock::given(method("POST"))
                .and(path("/v1/audio/transcriptions"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"text": "test transcript"})),
                )
                .mount(&mock_server)
                .await;

            let whisper_url = format!("{}/v1/audio/transcriptions", mock_server.uri());
            let ch = MattermostChannel::new(
                mock_server.uri(),
                "test_token".to_string(),
                None,
                vec!["*".into()],
                false,
                false,
            )
            .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                default_provider: "local_whisper".to_string(),
                api_key: None,
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_duration_secs: 600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                    url: whisper_url,
                    bearer_token: Some("test_token".to_string()),
                    max_audio_bytes: 25_000_000,
                    timeout_secs: 300,
                }),
                transcribe_non_ptt_audio: false,
            });

            let post = json!({
                "metadata": {
                    "files": [
                        {
                            "id": "file1",
                            "mime_type": "audio/ogg",
                            "name": "voice.ogg"
                        }
                    ]
                }
            });

            let result = ch.try_transcribe_audio_attachment(&post).await;
            assert_eq!(result.as_deref(), Some("[Voice] test transcript"));
        }

        #[tokio::test]
        async fn mattermost_audio_skips_non_audio_attachment() {
            let mock_server = MockServer::start().await;

            let ch = MattermostChannel::new(
                mock_server.uri(),
                "test_token".to_string(),
                None,
                vec!["*".into()],
                false,
                false,
            )
            .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                default_provider: "local_whisper".to_string(),
                api_key: None,
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_duration_secs: 600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                    url: mock_server.uri(),
                    bearer_token: Some("test_token".to_string()),
                    max_audio_bytes: 25_000_000,
                    timeout_secs: 300,
                }),
                transcribe_non_ptt_audio: false,
            });

            let post = json!({
                "metadata": {
                    "files": [
                        {
                            "id": "file1",
                            "mime_type": "image/png",
                            "name": "screenshot.png"
                        }
                    ]
                }
            });

            let result = ch.try_transcribe_audio_attachment(&post).await;
            assert!(result.is_none());
        }
    }

    // ── WebSocket helpers ────────────────────────────────────────

    #[test]
    fn ws_url_converts_https_to_wss() {
        let ch = MattermostChannel::new(
            "https://mm.example.com".into(),
            "token".into(),
            None,
            vec![],
            false,
            false,
        );
        assert_eq!(ch.ws_url(), "wss://mm.example.com/api/v4/websocket");
    }

    #[test]
    fn ws_url_converts_http_to_ws() {
        let ch = MattermostChannel::new(
            "http://localhost:8065".into(),
            "token".into(),
            None,
            vec![],
            false,
            false,
        );
        assert_eq!(ch.ws_url(), "ws://localhost:8065/api/v4/websocket");
    }

    #[test]
    fn channel_matches_filter_wildcard() {
        let ch = MattermostChannel::new(
            "url".into(),
            "token".into(),
            Some("*".into()),
            vec![],
            false,
            false,
        );
        assert!(ch.channel_matches_filter("any_channel"));
    }

    #[test]
    fn channel_matches_filter_none() {
        let ch = MattermostChannel::new("url".into(), "token".into(), None, vec![], false, false);
        assert!(ch.channel_matches_filter("any_channel"));
    }

    #[test]
    fn channel_matches_filter_specific() {
        let ch = MattermostChannel::new(
            "url".into(),
            "token".into(),
            Some("chan123".into()),
            vec![],
            false,
            false,
        );
        assert!(ch.channel_matches_filter("chan123"));
        assert!(!ch.channel_matches_filter("other_chan"));
    }

    // ── DM and thread mention_only bypass ────────────────────────

    #[test]
    fn mention_only_dm_always_responds() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hello without mention",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "dm_chan1",
                "D",
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "hello without mention");
    }

    #[test]
    fn mention_only_group_still_requires_mention() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hello without mention",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "mybot",
            1_500_000_000_000_i64,
            "chan1",
            "O",
            None,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mention_only_thread_reply_always_responds() {
        let ch = make_mention_only_channel();
        let post = json!({
            "id": "post2",
            "user_id": "user1",
            "message": "follow-up without mention",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "post1"
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                "O",
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "follow-up without mention");
        assert_eq!(msg.reply_target, "chan1:post1");
    }

    // ── WebSocket integration tests ──────────────────────────────

    async fn run_ws_test(
        channel_id: Option<String>,
        mention_only: bool,
        allowed_users: Vec<String>,
        bot_user_id: &str,
        bot_username: &str,
        events: Vec<serde_json::Value>,
    ) -> Vec<ChannelMessage> {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let bot_id = bot_user_id.to_string();
        let bot_name = bot_username.to_string();
        let events_clone = events.clone();

        let ws_handle = tokio::spawn(async move {
            // 1) REST: /api/v4/users/me
            let (mut rest_stream, _) = listener.accept().await.unwrap();
            let mut rest_buf = vec![0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut rest_stream, &mut rest_buf).await;
            let body = serde_json::json!({ "id": bot_id, "username": bot_name });
            let body_str = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body_str.len(),
                body_str
            );
            tokio::io::AsyncWriteExt::write_all(&mut rest_stream, response.as_bytes())
                .await
                .unwrap();
            drop(rest_stream);

            // 2) WS: accept WebSocket upgrade
            let (ws_tcp, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(ws_tcp).await.unwrap();
            let (mut write, mut read) = futures_util::StreamExt::split(ws_stream);

            let _auth = futures_util::StreamExt::next(&mut read).await;

            let ok = serde_json::json!({"status": "OK", "seq_reply": 1});
            futures_util::SinkExt::send(&mut write, WsMsg::Text(ok.to_string().into()))
                .await
                .unwrap();

            for event in events_clone {
                futures_util::SinkExt::send(&mut write, WsMsg::Text(event.to_string().into()))
                    .await
                    .unwrap();
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = futures_util::SinkExt::send(&mut write, WsMsg::Close(None)).await;
        });

        let ch = MattermostChannel::new(
            format!("http://{addr}"),
            "test_token".into(),
            channel_id,
            allowed_users,
            true,
            mention_only,
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let _result = ch.listen_websocket(tx).await;
        ws_handle.await.unwrap();

        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }
        msgs
    }

    fn make_ws_posted_event(
        post_id: &str,
        user_id: &str,
        channel_id: &str,
        channel_type: &str,
        message: &str,
    ) -> serde_json::Value {
        let post = serde_json::json!({
            "id": post_id,
            "user_id": user_id,
            "channel_id": channel_id,
            "message": message,
            "create_at": 1_700_000_000_000_i64,
            "root_id": "",
            "metadata": {}
        });
        serde_json::json!({
            "event": "posted",
            "data": {
                "post": serde_json::to_string(&post).unwrap(),
                "channel_type": channel_type
            },
            "broadcast": { "channel_id": channel_id }
        })
    }

    #[tokio::test]
    async fn ws_receives_posted_event() {
        let events = vec![make_ws_posted_event(
            "p1", "user1", "chan1", "O", "hello ws",
        )];
        let msgs = run_ws_test(None, false, vec!["*".into()], "bot1", "botname", events).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello ws");
        assert_eq!(msgs[0].sender, "user1");
    }

    #[tokio::test]
    async fn ws_filters_by_channel_id() {
        let events = vec![
            make_ws_posted_event("p1", "user1", "chan1", "O", "yes"),
            make_ws_posted_event("p2", "user1", "chan2", "O", "no"),
        ];
        let msgs = run_ws_test(
            Some("chan1".into()),
            false,
            vec!["*".into()],
            "bot1",
            "botname",
            events,
        )
        .await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "yes");
    }

    #[tokio::test]
    async fn ws_ignores_bot_own_messages() {
        let events = vec![make_ws_posted_event(
            "p1",
            "bot1",
            "chan1",
            "O",
            "my own msg",
        )];
        let msgs = run_ws_test(None, false, vec!["*".into()], "bot1", "botname", events).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn ws_dm_bypasses_mention_only() {
        let events = vec![make_ws_posted_event(
            "p1",
            "user1",
            "dm_chan",
            "D",
            "hello without mention",
        )];
        let msgs = run_ws_test(None, true, vec!["*".into()], "bot1", "botname", events).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello without mention");
    }

    #[tokio::test]
    async fn ws_group_requires_mention_when_mention_only() {
        let events = vec![make_ws_posted_event(
            "p1",
            "user1",
            "chan1",
            "O",
            "hello without mention",
        )];
        let msgs = run_ws_test(None, true, vec!["*".into()], "bot1", "botname", events).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn ws_wildcard_receives_all_channels() {
        let events = vec![
            make_ws_posted_event("p1", "user1", "chan1", "O", "msg1"),
            make_ws_posted_event("p2", "user1", "chan2", "P", "msg2"),
            make_ws_posted_event("p3", "user1", "dm1", "D", "msg3"),
        ];
        let msgs = run_ws_test(
            Some("*".into()),
            false,
            vec!["*".into()],
            "bot1",
            "botname",
            events,
        )
        .await;
        assert_eq!(msgs.len(), 3);
    }
}
