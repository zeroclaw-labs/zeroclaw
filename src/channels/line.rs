use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Result;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::Path;

pub use crate::config::schema::LineReplyMode;

/// LINE Messaging API base URL
const LINE_API_BASE: &str = "https://api.line.me/v2/bot";

/// Maximum LINE text message length
const LINE_MAX_TEXT_LEN: usize = 5000;

/// LINE channel max image download size (10 MB)
const LINE_MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

// ── Signature verification ────────────────────────────────────────────────────

/// Verify the `X-Line-Signature` header using HMAC-SHA256 + Base64.
///
/// The gateway calls this before processing any webhook body.
pub fn verify_line_signature(channel_secret: &str, body: &str, signature: &str) -> bool {
    if signature.is_empty() {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(channel_secret.as_bytes()) else {
        return false;
    };
    mac.update(body.as_bytes());
    let result = mac.finalize().into_bytes();
    let expected = general_purpose::STANDARD.encode(result);
    // Constant-time compare is not strictly required here because LINE sends
    // the expected value to us (not the other way around), but we keep it
    // simple and secure.
    expected == signature
}

// ── Incoming event structs ────────────────────────────────────────────────────

/// Top-level webhook payload from LINE
#[derive(Debug, serde::Deserialize)]
struct LineWebhook {
    events: Vec<LineEvent>,
}

#[derive(Debug, serde::Deserialize)]
struct LineEvent {
    #[serde(rename = "type")]
    event_type: String,
    /// present on message events (renamed from replyToken in JSON)
    #[serde(rename = "replyToken")]
    token: Option<String>,
    source: LineSource,
    message: Option<LineMsg>,
    /// milliseconds since epoch
    timestamp: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
struct LineSource {
    #[serde(rename = "type")]
    source_type: String,
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
    #[serde(rename = "roomId")]
    room_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct LineMsg {
    id: String,
    #[serde(rename = "type")]
    msg_type: String,
    /// text message body (present when msg_type == "text")
    text: Option<String>,
}

// ── Outgoing message structs ──────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
struct LinePushRequest<'a> {
    to: &'a str,
    messages: Vec<LineTextMsg>,
}

#[derive(Debug, serde::Serialize)]
struct LineReplyRequest<'a> {
    #[serde(rename = "replyToken")]
    reply_token: &'a str,
    messages: Vec<LineTextMsg>,
}

#[derive(Debug, serde::Serialize)]
struct LineTextMsg {
    #[serde(rename = "type")]
    msg_type: &'static str,
    text: String,
}

// ── LineChannel ───────────────────────────────────────────────────────────────

/// LINE Messaging API channel — **webhook-based** (push-mode).
///
/// Messages are received via the gateway's `/line` endpoint.
/// The `listen` method is a keepalive placeholder; real message handling
/// happens in the gateway when LINE sends webhook events.
///
/// Incoming text messages and images from both 1:1 and group chats are
/// supported.  Images are downloaded from the Content API, saved to
/// `workspace/line_files/`, and represented as `[IMAGE:/path]` markers so
/// the multimodal pipeline can process them.
pub struct LineChannel {
    /// LINE Channel Access Token (used for all outbound API calls)
    channel_access_token: String,
    /// Allowed LINE user IDs. `["*"]` accepts all users.
    allowed_users: Vec<String>,
    /// Optional directory for storing downloaded image files.
    workspace_dir: Option<std::path::PathBuf>,
    /// Outbound delivery mode (reply_first / push_only / reply_only).
    reply_mode: LineReplyMode,
    client: reqwest::Client,
}

impl LineChannel {
    pub fn new(channel_access_token: String, allowed_users: Vec<String>) -> Self {
        Self {
            channel_access_token,
            allowed_users,
            workspace_dir: None,
            reply_mode: LineReplyMode::default(),
            client: reqwest::Client::new(),
        }
    }

    /// Set the outbound delivery mode.
    pub fn with_reply_mode(mut self, mode: LineReplyMode) -> Self {
        self.reply_mode = mode;
        self
    }

    /// Attach a workspace directory for saving downloaded images.
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    // ── Permissions ───────────────────────────────────────────────────────────

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn api_url(&self, path: &str) -> String {
        format!("{LINE_API_BASE}/{path}")
    }

    /// Build the *reply_target* for a source.
    ///
    /// For 1:1 chats the target is the userId; for groups/rooms we use the
    /// group/room ID so push messages land in the right conversation.
    fn reply_target(source: &LineSource) -> Option<String> {
        match source.source_type.as_str() {
            "group" => source.group_id.clone(),
            "room" => source.room_id.clone(),
            "user" => source.user_id.clone(),
            _ => None,
        }
    }

    // ── Image handling ────────────────────────────────────────────────────────

    /// Download a LINE image/video/audio message by its message ID and save it
    /// to disk.  Returns the local file path on success.
    async fn download_content(
        &self,
        message_id: &str,
        ext: &str,
        chat_id: &str,
    ) -> Option<std::path::PathBuf> {
        let workspace = self.workspace_dir.as_ref()?;
        let save_dir = workspace.join("line_files");

        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("LINE: failed to create line_files dir: {e}");
            return None;
        }

        let url = self.api_url(&format!("message/{message_id}/content"));
        let resp = match self
            .client
            .get(&url)
            .bearer_auth(&self.channel_access_token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("LINE: failed to fetch content for msg={message_id}: {e}");
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                "LINE: content API returned {} for msg={message_id}",
                resp.status()
            );
            return None;
        }

        // Guard against enormous files
        if let Some(len) = resp.content_length() {
            if len > LINE_MAX_IMAGE_BYTES {
                tracing::info!(
                    "LINE: skipping content {message_id}: {len} bytes > {} MB limit",
                    LINE_MAX_IMAGE_BYTES / (1024 * 1024)
                );
                return None;
            }
        }

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("LINE: failed to read content body: {e}");
                return None;
            }
        };

        let safe_chat_id = chat_id.replace(['-', ':'], "_");
        let filename = format!("line_{safe_chat_id}_{message_id}.{ext}");
        let local_path = save_dir.join(&filename);

        if let Err(e) = tokio::fs::write(&local_path, &bytes).await {
            tracing::warn!("LINE: failed to write {}: {e}", local_path.display());
            return None;
        }

        Some(local_path)
    }

    /// Decide the file extension for a given LINE message type.
    fn content_ext(msg_type: &str) -> &'static str {
        match msg_type {
            "image" => "jpg",
            "video" => "mp4",
            "audio" => "m4a",
            _ => "bin",
        }
    }

    /// Format a local file path into a content marker string.
    fn format_content_marker(msg_type: &str, path: &Path) -> String {
        let path_str = path.display().to_string();
        match msg_type {
            "image" => format!("[IMAGE:{path_str}]"),
            "video" => format!("[VIDEO:{path_str}]"),
            "audio" => format!("[AUDIO:{path_str}]"),
            _ => format!("[FILE:{path_str}]"),
        }
    }

    // ── Parse webhook into channel messages ───────────────────────────────────

    /// Parse the raw JSON webhook payload into zero or more `ChannelMessage`s.
    ///
    /// This is called by the gateway handler after signature verification.
    pub async fn parse_webhook_payload(&self, raw: &serde_json::Value) -> Vec<ChannelMessage> {
        let webhook: LineWebhook = match serde_json::from_value(raw.clone()) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("LINE: failed to parse webhook JSON: {e}");
                return vec![];
            }
        };

        let mut out = Vec::new();

        for event in webhook.events {
            if event.event_type != "message" {
                continue;
            }

            let Some(msg) = event.message else { continue };
            let Some(reply_target) = Self::reply_target(&event.source) else {
                continue;
            };

            // The sender is always the userId (even in groups)
            let sender = match event.source.user_id.as_deref() {
                Some(uid) => uid.to_string(),
                None => continue,
            };

            if !self.is_user_allowed(&sender) {
                tracing::debug!("LINE: ignoring msg from non-allowlisted user {sender}");
                continue;
            }

            let timestamp = event.timestamp.unwrap_or(0) / 1000;

            let content: Option<String> = match msg.msg_type.as_str() {
                "text" => msg.text.clone(),

                "image" | "video" | "audio" => {
                    let ext = Self::content_ext(&msg.msg_type);
                    if let Some(path) = self.download_content(&msg.id, ext, &reply_target).await {
                        Some(Self::format_content_marker(&msg.msg_type, &path))
                    } else {
                        // Fall back to a text description so the agent knows something arrived
                        Some(format!("[LINE {} received]", msg.msg_type))
                    }
                }

                "sticker" => Some("[Sticker 🎭]".to_string()),

                other => {
                    tracing::debug!("LINE: unsupported message type '{other}', skipping");
                    None
                }
            };

            let Some(content) = content else { continue };

            // Build a stable, deduplicated message ID using the LINE message ID
            let id = format!("line_{}", msg.id);

            // Store the replyToken in thread_ts so the gateway handler can pass it
            // to send_with_reply_token() without changing the ChannelMessage schema.
            // LINE reply tokens are one-time-use and expire after 1 minute.
            let reply_token = event.token.filter(|t| !t.is_empty());

            out.push(ChannelMessage {
                id,
                sender,
                reply_target,
                content,
                channel: "line".to_string(),
                timestamp,
                thread_ts: reply_token,
                interruption_scope_id: None,
                attachments: vec![],
            });
        }

        out
    }

    // ── Outbound: split long messages ─────────────────────────────────────────

    /// Split a long reply into 5000-char chunks (LINE's per-message limit).
    fn split_message(text: &str) -> Vec<String> {
        if text.chars().count() <= LINE_MAX_TEXT_LEN {
            return vec![text.to_string()];
        }
        let mut chunks = Vec::new();
        let mut remaining = text;
        while !remaining.is_empty() {
            let end = remaining
                .char_indices()
                .nth(LINE_MAX_TEXT_LEN)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
            chunks.push(remaining[..end].to_string());
            remaining = &remaining[end..];
        }
        chunks
    }

    // ── Outbound: Push API ────────────────────────────────────────────────────

    /// Send via Push API (`/message/push`). Consumes Push quota.
    async fn send_via_push(&self, to: &str, text: &str) -> Result<()> {
        for chunk in Self::split_message(text) {
            let payload = LinePushRequest {
                to,
                messages: vec![LineTextMsg {
                    msg_type: "text",
                    text: chunk,
                }],
            };
            let resp = self
                .client
                .post(self.api_url("message/push"))
                .bearer_auth(&self.channel_access_token)
                .json(&payload)
                .send()
                .await?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("LINE push API error: {status} — {body}");
            }
        }
        Ok(())
    }

    // ── Outbound: Reply API ───────────────────────────────────────────────────

    /// Send via Reply API (`/message/reply`). Free — no Push quota cost.
    ///
    /// LINE allows **at most 5 messages per reply token** and the token
    /// expires after **1 minute**. Only the first call per token is valid;
    /// subsequent calls with the same token will fail.
    async fn send_via_reply(&self, reply_token: &str, text: &str) -> Result<()> {
        let chunks = Self::split_message(text);
        // Reply API accepts up to 5 messages per call — send all at once.
        let messages: Vec<LineTextMsg> = chunks
            .into_iter()
            .map(|t| LineTextMsg {
                msg_type: "text",
                text: t,
            })
            .collect();
        let payload = LineReplyRequest {
            reply_token,
            messages,
        };
        let resp = self
            .client
            .post(self.api_url("message/reply"))
            .bearer_auth(&self.channel_access_token)
            .json(&payload)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("LINE reply API error: {status} — {body}");
        }
        Ok(())
    }

    // ── Outbound: mode-aware dispatch ─────────────────────────────────────────

    /// Send a reply honouring the configured [`LineReplyMode`].
    ///
    /// The `reply_token` comes from the incoming webhook event and is stored
    /// in `ChannelMessage::thread_ts` by [`parse_webhook_payload`].
    ///
    /// | Mode | Behaviour |
    /// |------|-----------|
    /// | `reply_first` | Reply API when token present; fallback to Push on error (e.g. expired token) |
    /// | `push_only`   | Always Push API |
    /// | `reply_only`  | Reply API; error if token absent |
    pub async fn send_with_reply_token(
        &self,
        message: &SendMessage,
        reply_token: Option<&str>,
    ) -> Result<()> {
        match self.reply_mode {
            LineReplyMode::ReplyFirst => {
                if let Some(token) = reply_token.filter(|t| !t.is_empty()) {
                    tracing::debug!("LINE: sending via Reply API");
                    match self.send_via_reply(token, &message.content).await {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            tracing::warn!(
                                "LINE: Reply API failed ({e:#}), falling back to Push API"
                            );
                            self.send_via_push(&message.recipient, &message.content)
                                .await
                        }
                    }
                } else {
                    tracing::debug!("LINE: no reply token — falling back to Push API");
                    self.send_via_push(&message.recipient, &message.content)
                        .await
                }
            }
            LineReplyMode::PushOnly => {
                tracing::debug!("LINE: push_only mode — using Push API");
                self.send_via_push(&message.recipient, &message.content)
                    .await
            }
            LineReplyMode::ReplyOnly => {
                let token = reply_token.filter(|t| !t.is_empty()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "LINE reply_only mode but no replyToken available for recipient {}",
                        message.recipient
                    )
                })?;
                tracing::debug!("LINE: reply_only mode — using Reply API");
                self.send_via_reply(token, &message.content).await
            }
        }
    }
}

// ── Channel trait ─────────────────────────────────────────────────────────────

#[async_trait]
impl Channel for LineChannel {
    fn name(&self) -> &str {
        "line"
    }

    /// Send via Push API — generic trait implementation (no reply token context).
    ///
    /// The gateway LINE handler uses [`LineChannel::send_with_reply_token`] instead,
    /// which honours the configured [`LineReplyMode`]. This trait impl is kept for
    /// programmatic / proactive sends that have no incoming `replyToken`.
    async fn send(&self, message: &SendMessage) -> Result<()> {
        self.send_via_push(&message.recipient, &message.content)
            .await
    }

    /// LINE is webhook-based — this loop is a keepalive placeholder only.
    ///
    /// Actual inbound messages are processed by the gateway's `/line` handler,
    /// which calls `parse_webhook_payload`.
    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!("LINE channel active (webhook mode — gateway handles inbound events).");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Call getProfile on ourselves; any 200 from the API means the token works
        let url = self.api_url("info");
        match self
            .client
            .get(&url)
            .bearer_auth(&self.channel_access_token)
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> LineChannel {
        LineChannel::new("test-token".to_string(), vec!["U123".to_string()])
    }

    #[test]
    fn verify_signature_correct() {
        // Pre-computed: HMAC-SHA256("secret", "{\"events\":[]}") → base64
        let body = "{\"events\":[]}";
        let secret = "secret";
        let sig = {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(body.as_bytes());
            general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        };
        assert!(verify_line_signature(secret, body, &sig));
    }

    #[test]
    fn verify_signature_wrong() {
        assert!(!verify_line_signature("secret", "body", "badsig"));
        assert!(!verify_line_signature("secret", "body", ""));
    }

    #[test]
    fn is_user_allowed_wildcard() {
        let ch = LineChannel::new("tok".to_string(), vec!["*".to_string()]);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn is_user_allowed_specific() {
        let ch = make_channel();
        assert!(ch.is_user_allowed("U123"));
        assert!(!ch.is_user_allowed("U999"));
    }

    #[test]
    fn split_message_short() {
        let chunks = LineChannel::split_message("hello");
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long() {
        let long_text = "a".repeat(LINE_MAX_TEXT_LEN + 10);
        let chunks = LineChannel::split_message(&long_text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), LINE_MAX_TEXT_LEN);
    }

    #[tokio::test]
    async fn parse_empty_events() {
        let ch = make_channel();
        let raw = serde_json::json!({"events": []});
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn parse_text_message_allowed() {
        let ch = make_channel();
        let raw = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "tok",
                "timestamp": 1_700_000_000_000u64,
                "source": { "type": "user", "userId": "U123" },
                "message": { "id": "m1", "type": "text", "text": "hello bot" }
            }]
        });
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello bot");
        assert_eq!(msgs[0].sender, "U123");
        assert_eq!(msgs[0].reply_target, "U123");
        assert_eq!(msgs[0].channel, "line");
    }

    #[tokio::test]
    async fn parse_text_message_denied() {
        let ch = make_channel();
        let raw = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "tok",
                "timestamp": 1_700_000_000_000u64,
                "source": { "type": "user", "userId": "U_BLOCKED" },
                "message": { "id": "m2", "type": "text", "text": "hack" }
            }]
        });
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert!(msgs.is_empty(), "blocked user must not produce messages");
    }

    #[tokio::test]
    async fn parse_group_message() {
        let ch = LineChannel::new("tok".to_string(), vec!["*".to_string()]);
        let raw = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "tok",
                "timestamp": 1_700_000_000_000u64,
                "source": {
                    "type": "group",
                    "groupId": "C_GROUP",
                    "userId": "U_MEMBER"
                },
                "message": { "id": "m3", "type": "text", "text": "group msg" }
            }]
        });
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert_eq!(msgs.len(), 1);
        // reply_target for groups must be the groupId, not the userId
        assert_eq!(msgs[0].reply_target, "C_GROUP");
        assert_eq!(msgs[0].sender, "U_MEMBER");
    }

    #[tokio::test]
    async fn parse_sticker() {
        let ch = LineChannel::new("tok".to_string(), vec!["*".to_string()]);
        let raw = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "tok",
                "timestamp": 1_700_000_000_000u64,
                "source": { "type": "user", "userId": "U999" },
                "message": { "id": "m4", "type": "sticker" }
            }]
        });
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("Sticker"));
    }

    #[tokio::test]
    async fn parse_non_message_event_ignored() {
        let ch = make_channel();
        let raw = serde_json::json!({
            "events": [{
                "type": "follow",
                "timestamp": 1_700_000_000_000u64,
                "source": { "type": "user", "userId": "U123" }
            }]
        });
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert!(msgs.is_empty());
    }

    // ── reply_mode tests ──────────────────────────────────────────────────────

    #[test]
    fn default_reply_mode_is_reply_first() {
        let ch = make_channel();
        assert_eq!(ch.reply_mode, LineReplyMode::ReplyFirst);
    }

    #[test]
    fn with_reply_mode_sets_mode() {
        let ch = LineChannel::new("tok".to_string(), vec!["*".to_string()])
            .with_reply_mode(LineReplyMode::PushOnly);
        assert_eq!(ch.reply_mode, LineReplyMode::PushOnly);
    }

    #[tokio::test]
    async fn parse_stores_reply_token_in_thread_ts() {
        let ch = make_channel();
        let raw = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "my-reply-token",
                "timestamp": 1_700_000_000_000u64,
                "source": { "type": "user", "userId": "U123" },
                "message": { "id": "m1", "type": "text", "text": "hi" }
            }]
        });
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].thread_ts.as_deref(), Some("my-reply-token"));
    }

    #[tokio::test]
    async fn parse_empty_reply_token_stored_as_none() {
        let ch = make_channel();
        let raw = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "",
                "timestamp": 1_700_000_000_000u64,
                "source": { "type": "user", "userId": "U123" },
                "message": { "id": "m2", "type": "text", "text": "hi" }
            }]
        });
        let msgs = ch.parse_webhook_payload(&raw).await;
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].thread_ts.is_none(),
            "empty token must be stored as None"
        );
    }

    #[tokio::test]
    async fn reply_only_mode_errors_without_token() {
        let ch = LineChannel::new("tok".to_string(), vec!["*".to_string()])
            .with_reply_mode(LineReplyMode::ReplyOnly);
        let msg = crate::channels::traits::SendMessage::new("hello", "U999");
        let result = ch.send_with_reply_token(&msg, None).await;
        assert!(result.is_err(), "reply_only without token must return Err");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("reply_only mode but no replyToken")
        );
    }
}
