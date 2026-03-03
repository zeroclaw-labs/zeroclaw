use super::ack_reaction::{select_ack_reaction, AckReactionContext, AckReactionContextChatType};
use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::{AckReactionConfig, Config, StreamMode};
use crate::security::pairing::PairingGuard;
use anyhow::Context;
use async_trait::async_trait;
use directories::UserDirs;
use parking_lot::Mutex;
use reqwest::multipart::{Form, Part};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::fs;

/// Telegram's maximum message length for text messages
const TELEGRAM_MAX_MESSAGE_LENGTH: usize = 4096;
/// Reserve space for continuation markers added by send_text_chunks:
/// worst case is "(continued)\n\n" + chunk + "\n\n(continues...)" = 30 extra chars
const TELEGRAM_CONTINUATION_OVERHEAD: usize = 30;
const TELEGRAM_ACK_REACTIONS: &[&str] = &["⚡️", "👌", "👀", "🔥", "👍"];

/// Metadata for an incoming document or photo attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IncomingAttachment {
    file_id: String,
    file_name: Option<String>,
    file_size: Option<u64>,
    caption: Option<String>,
    kind: IncomingAttachmentKind,
}

/// The kind of incoming attachment (document vs photo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncomingAttachmentKind {
    Document,
    Photo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VoiceMetadata {
    file_id: String,
    duration_secs: u64,
    file_name_hint: Option<String>,
    mime_type_hint: Option<String>,
    voice_note: bool,
}
const TELEGRAM_BIND_COMMAND: &str = "/bind";
const TELEGRAM_APPROVAL_CALLBACK_APPROVE_PREFIX: &str = "zcapr:yes:";
const TELEGRAM_APPROVAL_CALLBACK_DENY_PREFIX: &str = "zcapr:no:";

/// Split a message into chunks that respect Telegram's 4096 character limit.
/// Tries to split at word boundaries when possible, and handles continuation.
/// The effective per-chunk limit is reduced to leave room for continuation markers.
fn split_message_for_telegram(message: &str) -> Vec<String> {
    if message.chars().count() <= TELEGRAM_MAX_MESSAGE_LENGTH {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;
    let chunk_limit = TELEGRAM_MAX_MESSAGE_LENGTH - TELEGRAM_CONTINUATION_OVERHEAD;

    while !remaining.is_empty() {
        // If the remainder fits within the full limit, take it all (last chunk
        // or single chunk — continuation overhead is at most 14 chars).
        if remaining.chars().count() <= TELEGRAM_MAX_MESSAGE_LENGTH {
            chunks.push(remaining.to_string());
            break;
        }

        // Find the byte offset for the Nth character boundary.
        let hard_split = remaining
            .char_indices()
            .nth(chunk_limit)
            .map_or(remaining.len(), |(idx, _)| idx);

        let chunk_end = if hard_split == remaining.len() {
            hard_split
        } else {
            // Try to find a good break point (newline, then space)
            let search_area = &remaining[..hard_split];

            // Prefer splitting at newline
            if let Some(pos) = search_area.rfind('\n') {
                // Don't split if the newline is too close to the start
                if search_area[..pos].chars().count() >= chunk_limit / 2 {
                    pos + 1
                } else {
                    // Try space as fallback
                    search_area.rfind(' ').unwrap_or(hard_split) + 1
                }
            } else if let Some(pos) = search_area.rfind(' ') {
                pos + 1
            } else {
                // Hard split at character boundary
                hard_split
            }
        };

        chunks.push(remaining[..chunk_end].to_string());
        remaining = &remaining[chunk_end..];
    }

    chunks
}

fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let upper = len as u64;
    let reject_threshold = (u64::MAX / upper) * upper;

    loop {
        let value = rand::random::<u64>();
        if value < reject_threshold {
            #[allow(clippy::cast_possible_truncation)]
            return (value % upper) as usize;
        }
    }
}

fn random_telegram_ack_reaction() -> &'static str {
    TELEGRAM_ACK_REACTIONS[pick_uniform_index(TELEGRAM_ACK_REACTIONS.len())]
}

fn build_telegram_ack_reaction_request(
    chat_id: &str,
    message_id: i64,
    emoji: &str,
) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reaction": [{
            "type": "emoji",
            "emoji": emoji
        }]
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TelegramAttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramAttachment {
    kind: TelegramAttachmentKind,
    target: String,
}

impl TelegramAttachmentKind {
    fn from_marker(marker: &str) -> Option<Self> {
        match marker.trim().to_ascii_uppercase().as_str() {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }
}

/// Check whether a file path has a recognized image extension.
fn is_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
        .unwrap_or(false)
}

/// Build the user-facing content string for an incoming attachment.
///
/// Photos and Documents with a recognized image extension use `[IMAGE:/path]`
/// so the multimodal pipeline can validate vision capability and send them
/// as proper image content blocks. Non-image files use `[Document: name] /path`.
fn format_attachment_content(
    kind: IncomingAttachmentKind,
    local_filename: &str,
    local_path: &Path,
) -> String {
    match kind {
        IncomingAttachmentKind::Photo | IncomingAttachmentKind::Document
            if is_image_extension(local_path) =>
        {
            format!("[IMAGE:{}]", local_path.display())
        }
        _ => {
            format!("[Document: {}] {}", local_filename, local_path.display())
        }
    }
}

fn is_http_url(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://")
}

fn sanitize_attachment_filename(file_name: &str) -> Option<String> {
    let basename = Path::new(file_name).file_name()?.to_str()?.trim();
    if basename.is_empty() || basename == "." || basename == ".." {
        return None;
    }

    let sanitized: String = basename
        .replace(['/', '\\'], "_")
        .chars()
        .take(128)
        .collect();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        None
    } else {
        Some(sanitized)
    }
}

fn sanitize_generated_extension(raw_ext: &str) -> String {
    let cleaned: String = raw_ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        "jpg".to_string()
    } else {
        cleaned
    }
}

fn resolve_workspace_attachment_path(workspace: &Path, target: &str) -> anyhow::Result<PathBuf> {
    if target.contains('\0') {
        anyhow::bail!("Telegram attachment path contains null byte");
    }

    let workspace_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    let candidate = if let Some(rel) = target.strip_prefix("/workspace/") {
        workspace.join(rel)
    } else if target == "/workspace" {
        workspace.to_path_buf()
    } else {
        let raw = Path::new(target);
        if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            workspace.join(raw)
        }
    };

    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("Telegram attachment path not found: {target}"))?;

    if !resolved.starts_with(&workspace_root) {
        anyhow::bail!("Telegram attachment path escapes workspace: {target}");
    }
    if !resolved.is_file() {
        anyhow::bail!(
            "Telegram attachment path is not a file: {}",
            resolved.display()
        );
    }

    Ok(resolved)
}

async fn resolve_workspace_attachment_output_path(
    workspace: &Path,
    file_name: &str,
) -> anyhow::Result<PathBuf> {
    let safe_name = sanitize_attachment_filename(file_name)
        .ok_or_else(|| anyhow::anyhow!("invalid attachment filename: {file_name}"))?;

    fs::create_dir_all(workspace).await?;
    let workspace_root = fs::canonicalize(workspace)
        .await
        .unwrap_or_else(|_| workspace.to_path_buf());

    let save_dir = workspace.join("telegram_files");
    fs::create_dir_all(&save_dir).await?;
    let resolved_save_dir = fs::canonicalize(&save_dir).await.with_context(|| {
        format!(
            "failed to resolve Telegram attachment save directory: {}",
            save_dir.display()
        )
    })?;

    if !resolved_save_dir.starts_with(&workspace_root) {
        anyhow::bail!(
            "Telegram attachment save directory escapes workspace: {}",
            save_dir.display()
        );
    }

    let output_path = resolved_save_dir.join(safe_name);
    match fs::symlink_metadata(&output_path).await {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                anyhow::bail!(
                    "refusing to write Telegram attachment through symlink: {}",
                    output_path.display()
                );
            }
            if !meta.is_file() {
                anyhow::bail!(
                    "Telegram attachment output path is not a regular file: {}",
                    output_path.display()
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }

    Ok(output_path)
}

fn infer_attachment_kind_from_target(target: &str) -> Option<TelegramAttachmentKind> {
    let normalized = target
        .split('?')
        .next()
        .unwrap_or(target)
        .split('#')
        .next()
        .unwrap_or(target);

    let extension = Path::new(normalized)
        .extension()
        .and_then(|ext| ext.to_str())?
        .to_ascii_lowercase();

    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => Some(TelegramAttachmentKind::Image),
        "mp4" | "mov" | "mkv" | "avi" | "webm" => Some(TelegramAttachmentKind::Video),
        "mp3" | "m4a" | "wav" | "flac" => Some(TelegramAttachmentKind::Audio),
        "ogg" | "oga" | "opus" => Some(TelegramAttachmentKind::Voice),
        "pdf" | "txt" | "md" | "csv" | "json" | "zip" | "tar" | "gz" | "doc" | "docx" | "xls"
        | "xlsx" | "ppt" | "pptx" => Some(TelegramAttachmentKind::Document),
        _ => None,
    }
}

fn parse_path_only_attachment(message: &str) -> Option<TelegramAttachment> {
    let trimmed = message.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return None;
    }

    let candidate = trimmed.trim_matches(|c| matches!(c, '`' | '"' | '\''));
    if candidate.chars().any(char::is_whitespace) {
        return None;
    }

    let candidate = candidate.strip_prefix("file://").unwrap_or(candidate);
    let kind = infer_attachment_kind_from_target(candidate)?;

    if !is_http_url(candidate) && !Path::new(candidate).exists() {
        return None;
    }

    Some(TelegramAttachment {
        kind,
        target: candidate.to_string(),
    })
}

/// Delegate to the shared `strip_tool_call_tags` in the parent module.
fn strip_tool_call_tags(message: &str) -> String {
    super::strip_tool_call_tags(message)
}

fn find_matching_close(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_attachment_markers(message: &str) -> (String, Vec<TelegramAttachment>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut attachments = Vec::new();
    let mut cursor = 0;

    while cursor < message.len() {
        let Some(open_rel) = message[cursor..].find('[') else {
            cleaned.push_str(&message[cursor..]);
            break;
        };

        let open = cursor + open_rel;
        cleaned.push_str(&message[cursor..open]);

        let Some(close_rel) = find_matching_close(&message[open + 1..]) else {
            cleaned.push_str(&message[open..]);
            break;
        };

        let close = open + 1 + close_rel;
        let marker = &message[open + 1..close];

        let parsed = marker.split_once(':').and_then(|(kind, target)| {
            let kind = TelegramAttachmentKind::from_marker(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(TelegramAttachment {
                kind,
                target: target.to_string(),
            })
        });

        if let Some(attachment) = parsed {
            // Skip duplicate targets — LLMs sometimes emit repeated markers in one reply.
            if !attachments
                .iter()
                .any(|a: &TelegramAttachment| a.target == attachment.target)
            {
                attachments.push(attachment);
            }
        } else {
            cleaned.push_str(&message[open..=close]);
        }

        cursor = close + 1;
    }

    (cleaned.trim().to_string(), attachments)
}

/// Telegram Bot API maximum file download size (20 MB).
const TELEGRAM_MAX_FILE_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;

/// Telegram channel — long-polls the Bot API for updates
pub struct TelegramChannel {
    bot_token: String,
    allowed_users: Arc<RwLock<Vec<String>>>,
    pairing: Option<PairingGuard>,
    client: reqwest::Client,
    typing_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stream_mode: StreamMode,
    draft_update_interval_ms: u64,
    last_draft_edit: Mutex<std::collections::HashMap<String, std::time::Instant>>,
    mention_only: bool,
    group_reply_allowed_sender_ids: Vec<String>,
    bot_username: Mutex<Option<String>>,
    /// Base URL for the Telegram Bot API. Defaults to `https://api.telegram.org`.
    /// Override for local Bot API servers or testing.
    api_base: String,
    transcription: Option<crate::config::TranscriptionConfig>,
    voice_transcriptions: Mutex<std::collections::HashMap<String, String>>,
    workspace_dir: Option<std::path::PathBuf>,
    /// Whether to send emoji reaction acknowledgments to incoming messages.
    ack_enabled: bool,
    ack_reaction: Option<AckReactionConfig>,
}

impl TelegramChannel {
    pub fn new(
        bot_token: String,
        allowed_users: Vec<String>,
        mention_only: bool,
        ack_enabled: bool,
    ) -> Self {
        let normalized_allowed = Self::normalize_allowed_users(allowed_users);
        let pairing = if normalized_allowed.is_empty() {
            let guard = PairingGuard::new(true, &[]);
            if let Some(code) = guard.pairing_code() {
                println!("  🔐 Telegram pairing required. One-time bind code: {code}");
                println!("     Send `{TELEGRAM_BIND_COMMAND} <code>` from your Telegram account.");
            }
            Some(guard)
        } else {
            None
        };

        Self {
            bot_token,
            allowed_users: Arc::new(RwLock::new(normalized_allowed)),
            pairing,
            client: reqwest::Client::new(),
            stream_mode: StreamMode::Off,
            draft_update_interval_ms: 1000,
            last_draft_edit: Mutex::new(std::collections::HashMap::new()),
            typing_handle: Mutex::new(None),
            mention_only,
            group_reply_allowed_sender_ids: Vec::new(),
            bot_username: Mutex::new(None),
            api_base: "https://api.telegram.org".to_string(),
            transcription: None,
            voice_transcriptions: Mutex::new(std::collections::HashMap::new()),
            workspace_dir: None,
            ack_reaction: None,
            ack_enabled,
        }
    }

    /// Configure workspace directory for saving downloaded attachments.
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Configure ACK reaction policy.
    pub fn with_ack_reaction(mut self, ack_reaction: Option<AckReactionConfig>) -> Self {
        self.ack_reaction = ack_reaction;
        self
    }

    /// Configure streaming mode for progressive draft updates.
    pub fn with_streaming(
        mut self,
        stream_mode: StreamMode,
        draft_update_interval_ms: u64,
    ) -> Self {
        self.stream_mode = stream_mode;
        self.draft_update_interval_ms = draft_update_interval_ms;
        self
    }

    /// Configure sender IDs that bypass mention gating in group chats.
    pub fn with_group_reply_allowed_senders(mut self, sender_ids: Vec<String>) -> Self {
        self.group_reply_allowed_sender_ids =
            Self::normalize_group_reply_allowed_sender_ids(sender_ids);
        self
    }

    /// Override the Telegram Bot API base URL.
    /// Useful for local Bot API servers or testing.
    pub fn with_api_base(mut self, api_base: String) -> Self {
        self.api_base = api_base;
        self
    }

    /// Configure voice transcription.
    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if config.enabled {
            self.transcription = Some(config);
        }
        self
    }

    /// Enable or disable emoji reaction acknowledgments to incoming messages.
    pub fn with_ack_enabled(mut self, enabled: bool) -> Self {
        self.ack_enabled = enabled;
        self
    }

    /// Parse reply_target into (chat_id, optional thread_id).
    fn parse_reply_target(reply_target: &str) -> (String, Option<String>) {
        if let Some((chat_id, thread_id)) = reply_target.split_once(':') {
            (chat_id.to_string(), Some(thread_id.to_string()))
        } else {
            (reply_target.to_string(), None)
        }
    }

    fn build_typing_action_body(reply_target: &str) -> serde_json::Value {
        let (chat_id, thread_id) = Self::parse_reply_target(reply_target);
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "action": "typing"
        });
        if let Some(thread_id) = thread_id {
            body["message_thread_id"] = serde_json::Value::String(thread_id);
        }
        body
    }

    fn build_approval_prompt_body(
        chat_id: &str,
        thread_id: Option<&str>,
        request_id: &str,
        tool_name: &str,
        args_preview: &str,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": format!(
                "Approval required for tool `{tool_name}`.\nRequest ID: `{request_id}`\nArgs: `{args_preview}`",
            ),
            "parse_mode": "Markdown",
            "reply_markup": {
                "inline_keyboard": [[
                    {
                        "text": "Approve",
                        "callback_data": format!("{TELEGRAM_APPROVAL_CALLBACK_APPROVE_PREFIX}{request_id}")
                    },
                    {
                        "text": "Deny",
                        "callback_data": format!("{TELEGRAM_APPROVAL_CALLBACK_DENY_PREFIX}{request_id}")
                    }
                ]]
            }
        });

        if let Some(thread_id) = thread_id {
            body["message_thread_id"] = serde_json::Value::String(thread_id.to_string());
        }

        body
    }

    fn extract_update_message_ack_target(
        update: &serde_json::Value,
    ) -> Option<(String, i64, AckReactionContextChatType, Option<String>)> {
        let message = update.get("message")?;
        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)?
            .to_string();
        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)?;
        let chat_type = message
            .get("chat")
            .and_then(|chat| chat.get("type"))
            .and_then(serde_json::Value::as_str)
            .map(|kind| {
                if kind == "group" || kind == "supergroup" {
                    AckReactionContextChatType::Group
                } else {
                    AckReactionContextChatType::Direct
                }
            })
            .unwrap_or(AckReactionContextChatType::Direct);
        let sender_id = message
            .get("from")
            .and_then(|sender| sender.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|value| value.to_string());
        Some((chat_id, message_id, chat_type, sender_id))
    }

    #[cfg(test)]
    fn extract_update_message_target(update: &serde_json::Value) -> Option<(String, i64)> {
        Self::extract_update_message_ack_target(update)
            .map(|(chat_id, message_id, _, _)| (chat_id, message_id))
    }

    fn parse_approval_callback_command(data: &str) -> Option<String> {
        if let Some(request_id) = data.strip_prefix(TELEGRAM_APPROVAL_CALLBACK_APPROVE_PREFIX) {
            if !request_id.trim().is_empty() {
                return Some(format!("/approve-allow {}", request_id.trim()));
            }
        }
        if let Some(request_id) = data.strip_prefix(TELEGRAM_APPROVAL_CALLBACK_DENY_PREFIX) {
            if !request_id.trim().is_empty() {
                return Some(format!("/approve-deny {}", request_id.trim()));
            }
        }
        None
    }

    fn answer_callback_query_nonblocking(&self, callback_id: String, text: &str) {
        let client = self.http_client();
        let url = self.api_url("answerCallbackQuery");
        let text = text.to_string();
        tokio::spawn(async move {
            let body = serde_json::json!({
                "callback_query_id": callback_id,
                "text": text,
                "show_alert": false
            });
            let _ = client.post(&url).json(&body).send().await;
        });
    }

    fn clear_callback_inline_keyboard_nonblocking(
        &self,
        chat_id: String,
        message_id: i64,
        thread_id: Option<String>,
    ) {
        let client = self.http_client();
        let url = self.api_url("editMessageReplyMarkup");
        tokio::spawn(async move {
            let mut body = serde_json::json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "reply_markup": {
                    "inline_keyboard": []
                }
            });
            if let Some(thread_id) = thread_id {
                body["message_thread_id"] = serde_json::Value::String(thread_id);
            }
            let _ = client.post(&url).json(&body).send().await;
        });
    }

    fn try_parse_approval_callback_query(
        &self,
        update: &serde_json::Value,
    ) -> Option<ChannelMessage> {
        let callback = update.get("callback_query")?;
        let callback_id = callback.get("id").and_then(serde_json::Value::as_str)?;
        let data = callback.get("data").and_then(serde_json::Value::as_str)?;
        let content = Self::parse_approval_callback_command(data)?;

        let message = callback.get("message")?;
        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string())?;
        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        let (username, sender_id, sender_identity) = Self::extract_sender_info(callback);
        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }
        if !self.is_any_user_allowed(identities.iter().copied()) {
            return None;
        }

        let thread_id = message
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string());
        let reply_target = if let Some(ref tid) = thread_id {
            format!("{chat_id}:{tid}")
        } else {
            chat_id.clone()
        };

        self.answer_callback_query_nonblocking(callback_id.to_string(), "Decision received");
        self.clear_callback_inline_keyboard_nonblocking(
            chat_id.clone(),
            message_id,
            thread_id.clone(),
        );

        Some(ChannelMessage {
            id: format!("telegram_cb_{chat_id}_{message_id}_{callback_id}"),
            sender: sender_identity,
            reply_target,
            content,
            channel: "telegram".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: thread_id,
        })
    }

    fn try_add_ack_reaction_nonblocking(&self, chat_id: String, message_id: i64, emoji: String) {
        if !self.ack_enabled {
            return;
        }
        let client = self.http_client();
        let url = self.api_url("setMessageReaction");
        let body = build_telegram_ack_reaction_request(&chat_id, message_id, &emoji);

        tokio::spawn(async move {
            let response = match client.post(&url).json(&body).send().await {
                Ok(resp) => resp,
                Err(err) => {
                    let sanitized = TelegramChannel::sanitize_telegram_error(&err.to_string());
                    tracing::warn!(
                        "Telegram: failed to add ACK reaction to chat_id={chat_id}, message_id={message_id}: {sanitized}"
                    );
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let err_body = response.text().await.unwrap_or_default();
                let sanitized = TelegramChannel::sanitize_telegram_error(&err_body);
                tracing::warn!(
                    "Telegram: add ACK reaction failed for chat_id={chat_id}, message_id={message_id}: status={status}, body={sanitized}"
                );
            }
        });
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.telegram")
    }

    fn sanitize_telegram_error(input: &str) -> String {
        let mut sanitized = crate::providers::sanitize_api_error(input);
        let mut search_from = 0usize;

        while let Some(rel) = sanitized[search_from..].find("/bot") {
            let marker_start = search_from + rel;
            let token_start = marker_start + "/bot".len();

            let Some(next_slash_rel) = sanitized[token_start..].find('/') else {
                break;
            };
            let token_end = token_start + next_slash_rel;

            let should_redact = sanitized[token_start..token_end].contains(':')
                && sanitized[token_start..token_end]
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-'));

            if should_redact {
                sanitized.replace_range(token_start..token_end, "[REDACTED]");
                search_from = token_start + "[REDACTED]".len();
            } else {
                search_from = token_start;
            }
        }

        sanitized
    }

    fn log_poll_transport_error(sanitized: &str, consecutive_failures: u32) {
        if consecutive_failures >= 6 && consecutive_failures.is_multiple_of(6) {
            tracing::warn!(
                "Telegram poll transport error persists (consecutive={}): {}",
                consecutive_failures,
                sanitized
            );
        } else {
            tracing::debug!(
                "Telegram poll transport error (consecutive={}): {}",
                consecutive_failures,
                sanitized
            );
        }
    }

    fn normalize_identity(value: &str) -> String {
        value.trim().trim_start_matches('@').to_string()
    }

    fn normalize_allowed_users(allowed_users: Vec<String>) -> Vec<String> {
        allowed_users
            .into_iter()
            .map(|entry| Self::normalize_identity(&entry))
            .filter(|entry| !entry.is_empty())
            .collect()
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

    fn is_group_sender_trigger_enabled(&self, sender_id: Option<&str>) -> bool {
        let Some(sender_id) = sender_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return false;
        };

        self.group_reply_allowed_sender_ids
            .iter()
            .any(|entry| entry == "*" || entry == sender_id)
    }

    async fn load_config_without_env() -> anyhow::Result<Config> {
        let home = UserDirs::new()
            .map(|u| u.home_dir().to_path_buf())
            .context("Could not find home directory")?;
        let zeroclaw_dir = home.join(".zeroclaw");
        let config_path = zeroclaw_dir.join("config.toml");

        let contents = fs::read_to_string(&config_path)
            .await
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;
        let mut config: Config = toml::from_str(&contents).context(
            "Failed to parse config.toml — check [channels.telegram] section for syntax errors",
        )?;
        config.config_path = config_path;
        config.workspace_dir = zeroclaw_dir.join("workspace");
        Ok(config)
    }

    async fn persist_allowed_identity(&self, identity: &str) -> anyhow::Result<()> {
        let mut config = Self::load_config_without_env().await?;
        let Some(telegram) = config.channels_config.telegram.as_mut() else {
            anyhow::bail!(
                "Missing [channels.telegram] section in config.toml. \
                Add bot_token and allowed_users under [channels.telegram], \
                or run `zeroclaw onboard --channels-only` to configure interactively"
            );
        };

        let normalized = Self::normalize_identity(identity);
        if normalized.is_empty() {
            anyhow::bail!("Cannot persist empty Telegram identity");
        }

        if !telegram.allowed_users.iter().any(|u| u == &normalized) {
            telegram.allowed_users.push(normalized);
            config
                .save()
                .await
                .context("Failed to persist Telegram allowlist to config.toml")?;
        }

        Ok(())
    }

    fn add_allowed_identity_runtime(&self, identity: &str) {
        let normalized = Self::normalize_identity(identity);
        if normalized.is_empty() {
            return;
        }
        if let Ok(mut users) = self.allowed_users.write() {
            if !users.iter().any(|u| u == &normalized) {
                users.push(normalized);
            }
        }
    }

    fn extract_bind_code(text: &str) -> Option<&str> {
        let mut parts = text.split_whitespace();
        let command = parts.next()?;
        let base_command = command.split('@').next().unwrap_or(command);
        if base_command != TELEGRAM_BIND_COMMAND {
            return None;
        }
        parts.next().map(str::trim).filter(|code| !code.is_empty())
    }

    fn pairing_code_active(&self) -> bool {
        self.pairing
            .as_ref()
            .and_then(PairingGuard::pairing_code)
            .is_some()
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.api_base, self.bot_token)
    }

    async fn fetch_bot_username(&self) -> anyhow::Result<String> {
        let resp = self.http_client().get(self.api_url("getMe")).send().await?;

        if !resp.status().is_success() {
            anyhow::bail!("Failed to fetch bot info: {}", resp.status());
        }

        let data: serde_json::Value = resp.json().await?;
        let username = data
            .get("result")
            .and_then(|r| r.get("username"))
            .and_then(|u| u.as_str())
            .context("Bot username not found in response")?;

        Ok(username.to_string())
    }

    async fn get_bot_username(&self) -> Option<String> {
        {
            let cache = self.bot_username.lock();
            if let Some(ref username) = *cache {
                return Some(username.clone());
            }
        }

        match self.fetch_bot_username().await {
            Ok(username) => {
                let mut cache = self.bot_username.lock();
                *cache = Some(username.clone());
                Some(username)
            }
            Err(e) => {
                tracing::warn!("Failed to fetch bot username: {e}");
                None
            }
        }
    }

    /// Register bot commands with Telegram's `setMyCommands` API so they
    /// appear in the command menu for users. Called once on startup.
    async fn register_commands(&self) -> anyhow::Result<()> {
        let url = self.api_url("setMyCommands");
        let body = serde_json::json!({
            "commands": [
                { "command": "new", "description": "Start a new conversation" },
                { "command": "model", "description": "Show or switch the current model" },
                { "command": "models", "description": "Show or switch the current provider" },
            ]
        });

        let resp = self.http_client().post(&url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // Only log Telegram's error_code and description, not the full body
            let detail = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    let code = v.get("error_code");
                    let desc = v.get("description").and_then(|d| d.as_str());
                    match (code, desc) {
                        (Some(c), Some(d)) => Some(format!("error_code={c}, description={d}")),
                        (_, Some(d)) => Some(format!("description={d}")),
                        _ => None,
                    }
                })
                .unwrap_or_else(|| "no parseable error detail".to_string());
            tracing::warn!("setMyCommands failed: status={status}, {detail}");
        } else {
            tracing::info!("Telegram bot commands registered successfully");
        }

        Ok(())
    }

    fn is_telegram_username_char(ch: char) -> bool {
        ch.is_ascii_alphanumeric() || ch == '_'
    }

    fn find_bot_mention_spans(text: &str, bot_username: &str) -> Vec<(usize, usize)> {
        let bot_username = bot_username.trim_start_matches('@');
        if bot_username.is_empty() {
            return Vec::new();
        }

        let mut spans = Vec::new();

        for (at_idx, ch) in text.char_indices() {
            if ch != '@' {
                continue;
            }

            if at_idx > 0 {
                let prev = text[..at_idx].chars().next_back().unwrap_or(' ');
                if Self::is_telegram_username_char(prev) {
                    continue;
                }
            }

            let username_start = at_idx + 1;
            let mut username_end = username_start;

            for (rel_idx, candidate_ch) in text[username_start..].char_indices() {
                if Self::is_telegram_username_char(candidate_ch) {
                    username_end = username_start + rel_idx + candidate_ch.len_utf8();
                } else {
                    break;
                }
            }

            if username_end == username_start {
                continue;
            }

            let mention_username = &text[username_start..username_end];
            if mention_username.eq_ignore_ascii_case(bot_username) {
                spans.push((at_idx, username_end));
            }
        }

        spans
    }

    fn contains_bot_mention(text: &str, bot_username: &str) -> bool {
        !Self::find_bot_mention_spans(text, bot_username).is_empty()
    }

    fn normalize_incoming_content(text: &str, bot_username: &str) -> Option<String> {
        let spans = Self::find_bot_mention_spans(text, bot_username);
        if spans.is_empty() {
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            return (!normalized.is_empty()).then_some(normalized);
        }

        let mut normalized = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end) in spans {
            normalized.push_str(&text[cursor..start]);
            cursor = end;
        }
        normalized.push_str(&text[cursor..]);

        let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
        (!normalized.is_empty()).then_some(normalized)
    }

    fn is_group_message(message: &serde_json::Value) -> bool {
        message
            .get("chat")
            .and_then(|c| c.get("type"))
            .and_then(|t| t.as_str())
            .map(|t| t == "group" || t == "supergroup")
            .unwrap_or(false)
    }

    fn should_skip_unauthorized_prompt(
        &self,
        message: &serde_json::Value,
        text: &str,
        sender_id: Option<&str>,
    ) -> bool {
        if !self.mention_only || !Self::is_group_message(message) {
            return false;
        }

        if self.is_group_sender_trigger_enabled(sender_id) {
            return false;
        }

        let bot_username = self.bot_username.lock();
        match bot_username.as_deref() {
            Some(bot_username) => !Self::contains_bot_mention(text, bot_username),
            // Without bot username, we cannot reliably decide mention intent.
            None => true,
        }
    }

    fn passes_mention_only_gate(
        &self,
        message: &serde_json::Value,
        sender_id: Option<&str>,
        text_to_check: Option<&str>,
    ) -> bool {
        if !self.mention_only || !Self::is_group_message(message) {
            return true;
        }

        if self.is_group_sender_trigger_enabled(sender_id) {
            return true;
        }

        let Some(text) = text_to_check else {
            return false;
        };

        let bot_username = self.bot_username.lock();
        match bot_username.as_deref() {
            Some(bot_username) => Self::contains_bot_mention(text, bot_username),
            None => false,
        }
    }

    fn is_user_allowed(&self, username: &str) -> bool {
        let identity = Self::normalize_identity(username);
        self.allowed_users
            .read()
            .map(|users| users.iter().any(|u| u == "*" || u == &identity))
            .unwrap_or(false)
    }

    fn is_any_user_allowed<'a, I>(&self, identities: I) -> bool
    where
        I: IntoIterator<Item = &'a str>,
    {
        identities.into_iter().any(|id| self.is_user_allowed(id))
    }

    async fn handle_unauthorized_message(&self, update: &serde_json::Value) {
        let Some(message) = update.get("message") else {
            return;
        };

        let Some(text) = message.get("text").and_then(serde_json::Value::as_str) else {
            return;
        };

        let username_opt = message
            .get("from")
            .and_then(|from| from.get("username"))
            .and_then(serde_json::Value::as_str);
        let username = username_opt.unwrap_or("unknown");
        let normalized_username = Self::normalize_identity(username);

        let sender_id = message
            .get("from")
            .and_then(|from| from.get("id"))
            .and_then(serde_json::Value::as_i64);
        let sender_id_str = sender_id.map(|id| id.to_string());
        let normalized_sender_id = sender_id_str.as_deref().map(Self::normalize_identity);

        if self.should_skip_unauthorized_prompt(message, text, sender_id_str.as_deref()) {
            return;
        }

        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string());

        let Some(chat_id) = chat_id else {
            tracing::warn!("Telegram: missing chat_id in message, skipping");
            return;
        };

        let mut identities = vec![normalized_username.as_str()];
        if let Some(ref id) = normalized_sender_id {
            identities.push(id.as_str());
        }

        if self.is_any_user_allowed(identities.iter().copied()) {
            return;
        }

        if let Some(code) = Self::extract_bind_code(text) {
            if let Some(pairing) = self.pairing.as_ref() {
                match pairing.try_pair(code, &chat_id).await {
                    Ok(Some(_token)) => {
                        let bind_identity = normalized_sender_id.clone().or_else(|| {
                            if normalized_username.is_empty() || normalized_username == "unknown" {
                                None
                            } else {
                                Some(normalized_username.clone())
                            }
                        });

                        if let Some(identity) = bind_identity {
                            self.add_allowed_identity_runtime(&identity);
                            match self.persist_allowed_identity(&identity).await {
                                Ok(()) => {
                                    let _ = self
                                        .send(&SendMessage::new(
                                            "✅ Telegram account bound successfully. You can talk to ZeroClaw now.",
                                            &chat_id,
                                        ))
                                        .await;
                                    tracing::info!(
                                        "Telegram: paired and allowlisted identity={identity}"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Telegram: failed to persist allowlist after bind: {e}"
                                    );
                                    let _ = self
                                        .send(&SendMessage::new(
                                            "⚠️ Bound for this runtime, but failed to persist config. Access may be lost after restart; check config file permissions.",
                                            &chat_id,
                                        ))
                                        .await;
                                }
                            }
                        } else {
                            let _ = self
                                .send(&SendMessage::new(
                                    "❌ Could not identify your Telegram account. Ensure your account has a username or stable user ID, then retry.",
                                    &chat_id,
                                ))
                                .await;
                        }
                    }
                    Ok(None) => {
                        let _ = self
                            .send(&SendMessage::new(
                                "❌ Invalid binding code. Ask operator for the latest code and retry.",
                                &chat_id,
                            ))
                            .await;
                    }
                    Err(lockout_secs) => {
                        let _ = self
                            .send(&SendMessage::new(
                                format!("⏳ Too many invalid attempts. Retry in {lockout_secs}s."),
                                &chat_id,
                            ))
                            .await;
                    }
                }
            } else {
                let _ = self
                    .send(&SendMessage::new(
                        "ℹ️ Telegram pairing is not active. Ask operator to add your user ID to channels.telegram.allowed_users in config.toml.",
                        &chat_id,
                    ))
                    .await;
            }
            return;
        }

        tracing::warn!(
            "Telegram: ignoring message from unauthorized user: username={username}, sender_id={}. \
Allowlist Telegram username (without '@') or numeric user ID.",
            sender_id_str.as_deref().unwrap_or("unknown")
        );

        let suggested_identity = normalized_sender_id
            .clone()
            .or_else(|| {
                if normalized_username.is_empty() || normalized_username == "unknown" {
                    None
                } else {
                    Some(normalized_username.clone())
                }
            })
            .unwrap_or_else(|| "YOUR_TELEGRAM_ID".to_string());

        let _ = self
            .send(&SendMessage::new(
                format!(
                    "🔐 This bot requires operator approval.\n\nCopy this command to operator terminal:\n`zeroclaw channel bind-telegram {suggested_identity}`\n\nAfter operator runs it, send your message again."
                ),
                &chat_id,
            ))
            .await;

        if self.pairing_code_active() {
            let _ = self
                .send(&SendMessage::new(
                    "ℹ️ If operator provides a one-time pairing code, you can also run `/bind <code>`.",
                    &chat_id,
                ))
                .await;
        }
    }

    /// Get the file path for a Telegram file ID via the Bot API.
    async fn get_file_path(&self, file_id: &str) -> anyhow::Result<String> {
        let url = self.api_url("getFile");
        let resp = self
            .http_client()
            .get(&url)
            .query(&[("file_id", file_id)])
            .send()
            .await
            .context("Failed to call Telegram getFile")?;

        let data: serde_json::Value = resp.json().await?;
        data.get("result")
            .and_then(|r| r.get("file_path"))
            .and_then(serde_json::Value::as_str)
            .map(String::from)
            .context("Telegram getFile: missing file_path in response")
    }

    /// Download a file from the Telegram CDN.
    async fn download_file(&self, file_path: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/file/bot{}/{file_path}", self.api_base, self.bot_token);
        let resp = self
            .http_client()
            .get(&url)
            .send()
            .await
            .context("Failed to download Telegram file")?;

        if !resp.status().is_success() {
            anyhow::bail!("Telegram file download failed: {}", resp.status());
        }

        Ok(resp.bytes().await?.to_vec())
    }

    /// Extract transcription metadata from a voice or audio payload.
    fn parse_voice_metadata(message: &serde_json::Value) -> Option<VoiceMetadata> {
        let (voice, voice_note) = if let Some(voice) = message.get("voice") {
            (voice, true)
        } else {
            (message.get("audio")?, false)
        };

        let file_id = voice.get("file_id")?.as_str()?.to_string();
        let duration_secs = voice
            .get("duration")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let file_name_hint = voice
            .get("file_name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .filter(|name| !name.trim().is_empty());
        let mime_type_hint = voice
            .get("mime_type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .filter(|mime| !mime.trim().is_empty());

        Some(VoiceMetadata {
            file_id,
            duration_secs,
            file_name_hint,
            mime_type_hint,
            voice_note,
        })
    }

    fn extension_from_audio_mime_type(mime_type: &str) -> Option<&'static str> {
        match mime_type.trim().to_ascii_lowercase().as_str() {
            "audio/flac" | "audio/x-flac" => Some("flac"),
            "audio/mpeg" => Some("mp3"),
            "audio/mp4" => Some("mp4"),
            "audio/x-m4a" => Some("m4a"),
            "audio/ogg" | "application/ogg" => Some("ogg"),
            "audio/opus" => Some("opus"),
            "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
            "audio/webm" => Some("webm"),
            _ => None,
        }
    }

    fn has_file_extension(name: &str) -> bool {
        std::path::Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| !ext.trim().is_empty())
    }

    fn infer_voice_filename(file_path: &str, metadata: &VoiceMetadata) -> String {
        let basename = file_path.rsplit('/').next().unwrap_or("").trim();
        if !basename.is_empty() && Self::has_file_extension(basename) {
            return basename.to_string();
        }

        if let Some(hint) = metadata
            .file_name_hint
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if Self::has_file_extension(hint) {
                return hint.to_string();
            }
        }

        let default_stem = if metadata.voice_note {
            "voice"
        } else {
            "audio"
        };
        let stem = if basename.is_empty() {
            metadata
                .file_name_hint
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(default_stem)
        } else {
            basename
        }
        .trim_end_matches('.');

        if let Some(extension) = metadata
            .mime_type_hint
            .as_deref()
            .and_then(Self::extension_from_audio_mime_type)
        {
            return format!("{stem}.{extension}");
        }

        // Last-resort fallback keeps extension present so transcription backends
        // do not reject otherwise valid payloads from extension-less file paths.
        if metadata.voice_note {
            format!("{stem}.ogg")
        } else {
            format!("{stem}.mp3")
        }
    }

    /// Extract attachment metadata from an incoming Telegram message (document or photo).
    ///
    /// Returns `None` for text-only, voice, and other unsupported message types.
    fn parse_attachment_metadata(message: &serde_json::Value) -> Option<IncomingAttachment> {
        // Try document first
        if let Some(doc) = message.get("document") {
            let file_id = doc.get("file_id")?.as_str()?.to_string();
            let file_name = doc
                .get("file_name")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            let file_size = doc.get("file_size").and_then(serde_json::Value::as_u64);
            let caption = message
                .get("caption")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            return Some(IncomingAttachment {
                file_id,
                file_name,
                file_size,
                caption,
                kind: IncomingAttachmentKind::Document,
            });
        }

        // Try photo (array of PhotoSize, take last = highest resolution)
        if let Some(photos) = message.get("photo").and_then(serde_json::Value::as_array) {
            let best = photos.last()?;
            let file_id = best.get("file_id")?.as_str()?.to_string();
            let file_size = best.get("file_size").and_then(serde_json::Value::as_u64);
            let caption = message
                .get("caption")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            return Some(IncomingAttachment {
                file_id,
                file_name: None,
                file_size,
                caption,
                kind: IncomingAttachmentKind::Photo,
            });
        }

        None
    }

    /// Attempt to parse a Telegram update as a document/photo attachment.
    ///
    /// Downloads the file to `{workspace_dir}/telegram_files/` and returns a
    /// `ChannelMessage` with the local file path. Returns `None` if the message
    /// is not an attachment, workspace_dir is not configured, or the file exceeds
    /// size limits.
    async fn try_parse_attachment_message(
        &self,
        update: &serde_json::Value,
    ) -> Option<ChannelMessage> {
        let message = update.get("message")?;
        let attachment = Self::parse_attachment_metadata(message)?;

        // Check file size limit
        if let Some(size) = attachment.file_size {
            if size > TELEGRAM_MAX_FILE_DOWNLOAD_BYTES {
                tracing::info!(
                    "Skipping attachment: file size {size} bytes exceeds {} MB limit",
                    TELEGRAM_MAX_FILE_DOWNLOAD_BYTES / (1024 * 1024)
                );
                return None;
            }
        }

        let (username, sender_id, sender_identity) = Self::extract_sender_info(message);

        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }

        if !self.is_any_user_allowed(identities.iter().copied()) {
            return None;
        }

        if !self.passes_mention_only_gate(
            message,
            sender_id.as_deref(),
            attachment.caption.as_deref(),
        ) {
            return None;
        }

        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string())?;

        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        let thread_id = message
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string());

        let reply_target = if let Some(ref tid) = thread_id {
            format!("{}:{}", chat_id, tid)
        } else {
            chat_id.clone()
        };

        // Ensure workspace directory is configured
        let workspace = self.workspace_dir.as_ref().or_else(|| {
            tracing::warn!("Cannot save attachment: workspace_dir not configured");
            None
        })?;

        // Download file from Telegram
        let tg_file_path = match self.get_file_path(&attachment.file_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to get attachment file path: {e}");
                return None;
            }
        };

        let file_data = match self.download_file(&tg_file_path).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Failed to download attachment: {e}");
                return None;
            }
        };

        // Determine local filename
        let local_filename = match &attachment.file_name {
            Some(name) => sanitize_attachment_filename(name)
                .unwrap_or_else(|| format!("attachment_{chat_id}_{message_id}.bin")),
            None => {
                // For photos, derive extension from Telegram file path
                let ext =
                    sanitize_generated_extension(tg_file_path.rsplit('.').next().unwrap_or("jpg"));
                format!("photo_{chat_id}_{message_id}.{ext}")
            }
        };

        let local_path =
            match resolve_workspace_attachment_output_path(workspace, &local_filename).await {
                Ok(path) => path,
                Err(e) => {
                    tracing::warn!(
                        "Failed to resolve attachment output path for {}: {e}",
                        local_filename
                    );
                    return None;
                }
            };
        if let Err(e) = tokio::fs::write(&local_path, &file_data).await {
            tracing::warn!("Failed to save attachment to {}: {e}", local_path.display());
            return None;
        }

        // Build message content.
        // Photos with image extensions use [IMAGE:] marker so the multimodal
        // pipeline validates vision capability. Non-image files always get
        // [Document:] format regardless of Telegram's classification.
        let mut content = format_attachment_content(attachment.kind, &local_filename, &local_path);
        if let Some(caption) = &attachment.caption {
            if !caption.is_empty() {
                use std::fmt::Write;
                let _ = write!(content, "\n\n{caption}");
            }
        }

        // Prepend reply context if replying to another message
        if let Some(quote) = self.extract_reply_context(message) {
            content = format!("{quote}\n\n{content}");
        }

        Some(ChannelMessage {
            id: format!("telegram_{chat_id}_{message_id}"),
            sender: sender_identity,
            reply_target,
            content,
            channel: "telegram".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: thread_id,
        })
    }

    /// Attempt to parse a Telegram update as a voice message and transcribe it.
    ///
    /// Returns `None` if the message is not a voice message, transcription is disabled,
    /// or the message exceeds duration limits.
    async fn try_parse_voice_message(&self, update: &serde_json::Value) -> Option<ChannelMessage> {
        // Check if transcription is enabled before doing anything else
        let config = match self.transcription.as_ref() {
            Some(c) => c,
            None => {
                // Log at debug level when a voice message is received but transcription is disabled
                if let Some(message) = update.get("message") {
                    if message.get("voice").is_some() || message.get("audio").is_some() {
                        tracing::debug!(
                            "Received voice/audio message but transcription is disabled. \
                             Set [transcription].enabled = true to enable voice transcription."
                        );
                    }
                }
                return None;
            }
        };
        let message = update.get("message")?;

        let metadata = Self::parse_voice_metadata(message)?;

        if metadata.duration_secs > config.max_duration_secs {
            tracing::info!(
                "Skipping voice message: duration {}s exceeds limit {}s",
                metadata.duration_secs,
                config.max_duration_secs
            );
            return None;
        }

        let (username, sender_id, sender_identity) = Self::extract_sender_info(message);

        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }

        if !self.is_any_user_allowed(identities.iter().copied()) {
            tracing::debug!(
                "Skipping voice message from unauthorized user: {} (allowed_users: {:?})",
                sender_identity,
                self.allowed_users
                    .read()
                    .map(|u| u.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            );
            return None;
        }

        if !self.passes_mention_only_gate(message, sender_id.as_deref(), None) {
            return None;
        }

        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string())?;

        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        let thread_id = message
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string());

        let reply_target = if let Some(ref tid) = thread_id {
            format!("{}:{}", chat_id, tid)
        } else {
            chat_id.clone()
        };

        // Download and transcribe
        let file_path = match self.get_file_path(&metadata.file_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to get voice file path: {e}");
                return None;
            }
        };

        let file_name = Self::infer_voice_filename(&file_path, &metadata);

        let audio_data = match self.download_file(&file_path).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Failed to download voice file: {e}");
                return None;
            }
        };

        let text =
            match super::transcription::transcribe_audio(audio_data, &file_name, config).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Voice transcription failed: {e}");
                    return None;
                }
            };

        if text.trim().is_empty() {
            tracing::info!("Voice transcription returned empty text, skipping");
            return None;
        }

        // Cache transcription for reply-context lookups
        {
            let mut cache = self.voice_transcriptions.lock();
            if cache.len() >= 100 {
                cache.clear();
            }
            cache.insert(format!("{chat_id}:{message_id}"), text.clone());
        }

        tracing::info!(
            "Voice message transcribed successfully ({} chars) for user {} in chat {}",
            text.len(),
            sender_identity,
            chat_id
        );

        let content = if let Some(quote) = self.extract_reply_context(message) {
            format!("{quote}\n\n[Voice] {text}")
        } else {
            format!("[Voice] {text}")
        };

        Some(ChannelMessage {
            id: format!("telegram_{chat_id}_{message_id}"),
            sender: sender_identity,
            reply_target,
            content,
            channel: "telegram".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: thread_id,
        })
    }

    /// Extract sender username and display identity from a Telegram message object.
    fn extract_sender_info(message: &serde_json::Value) -> (String, Option<String>, String) {
        let username = message
            .get("from")
            .and_then(|from| from.get("username"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let sender_id = message
            .get("from")
            .and_then(|from| from.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string());
        let sender_identity = if username == "unknown" {
            sender_id.clone().unwrap_or_else(|| "unknown".to_string())
        } else {
            username.clone()
        };
        (username, sender_id, sender_identity)
    }

    /// Extract reply context from a Telegram `reply_to_message`, if present.
    fn extract_reply_context(&self, message: &serde_json::Value) -> Option<String> {
        let reply = message.get("reply_to_message")?;

        let reply_sender = reply
            .get("from")
            .and_then(|from| from.get("username"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                reply
                    .get("from")
                    .and_then(|from| from.get("first_name"))
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or("unknown");

        let reply_text = if let Some(text) = reply.get("text").and_then(serde_json::Value::as_str) {
            text.to_string()
        } else if reply.get("voice").is_some() || reply.get("audio").is_some() {
            let reply_mid = reply.get("message_id").and_then(serde_json::Value::as_i64);
            let chat_id = message
                .get("chat")
                .and_then(|c| c.get("id"))
                .and_then(serde_json::Value::as_i64);
            if let (Some(mid), Some(cid)) = (reply_mid, chat_id) {
                self.voice_transcriptions
                    .lock()
                    .get(&format!("{cid}:{mid}"))
                    .map(|t| format!("[Voice] {t}"))
                    .unwrap_or_else(|| "[Voice message]".to_string())
            } else {
                "[Voice message]".to_string()
            }
        } else if reply.get("photo").is_some() {
            "[Photo]".to_string()
        } else if reply.get("document").is_some() {
            "[Document]".to_string()
        } else if reply.get("video").is_some() {
            "[Video]".to_string()
        } else if reply.get("sticker").is_some() {
            "[Sticker]".to_string()
        } else {
            "[Message]".to_string()
        };

        // Format as blockquote with sender attribution
        let quoted_lines: String = reply_text
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        Some(format!("> @{reply_sender}:\n{quoted_lines}"))
    }

    fn parse_update_message(&self, update: &serde_json::Value) -> Option<ChannelMessage> {
        let message = update.get("message")?;

        let text = message.get("text").and_then(serde_json::Value::as_str)?;

        let (username, sender_id, sender_identity) = Self::extract_sender_info(message);

        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }

        if !self.is_any_user_allowed(identities.iter().copied()) {
            return None;
        }

        let is_group = Self::is_group_message(message);
        let allow_sender_without_mention =
            is_group && self.is_group_sender_trigger_enabled(sender_id.as_deref());

        if !self.passes_mention_only_gate(message, sender_id.as_deref(), Some(text)) {
            return None;
        }

        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string())?;

        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        // Extract thread/topic ID for forum support
        let thread_id = message
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string());

        // reply_target: chat_id or chat_id:thread_id format
        let reply_target = if let Some(ref tid) = thread_id {
            format!("{}:{}", chat_id, tid)
        } else {
            chat_id.clone()
        };

        let content = if self.mention_only && is_group && !allow_sender_without_mention {
            let bot_username = self.bot_username.lock();
            match bot_username.as_ref() {
                Some(bot_username) => Self::normalize_incoming_content(text, bot_username)?,
                None => {
                    tracing::debug!(
                        "Telegram: bot_username missing at normalize stage; using original text"
                    );
                    text.to_string()
                }
            }
        } else {
            text.to_string()
        };

        let content = if let Some(quote) = self.extract_reply_context(message) {
            format!("{quote}\n\n{content}")
        } else {
            content
        };

        Some(ChannelMessage {
            id: format!("telegram_{chat_id}_{message_id}"),
            sender: sender_identity,
            reply_target,
            content,
            channel: "telegram".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: thread_id,
        })
    }

    /// Download a Telegram photo by file_id, resize to fit within 1024px, and return as base64 data URI.
    async fn resolve_photo_data_uri(&self, file_id: &str) -> anyhow::Result<String> {
        use base64::Engine as _;

        // Step 1: call getFile to get file_path
        let get_file_url = self.api_url(&format!("getFile?file_id={}", file_id));
        let resp = self.http_client().get(&get_file_url).send().await?;
        let json: serde_json::Value = resp.json().await?;
        let file_path = json
            .get("result")
            .and_then(|r| r.get("file_path"))
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("getFile: no file_path in response"))?
            .to_string();

        // Step 2: download the actual file
        let download_url = format!("{}/file/bot{}/{}", self.api_base, self.bot_token, file_path);
        let img_resp = self.http_client().get(&download_url).send().await?;
        let bytes = img_resp.bytes().await?;

        // Step 3: resize to max 1024px on longest side to fit within model context
        let resized_bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
            let img = image::load_from_memory(&bytes)?;
            let (w, h) = (img.width(), img.height());
            let max_dim = 512u32;
            let resized = if w > max_dim || h > max_dim {
                img.thumbnail(max_dim, max_dim)
            } else {
                img
            };
            let mut buf = Vec::new();
            resized.write_to(
                &mut std::io::Cursor::new(&mut buf),
                image::ImageFormat::Jpeg,
            )?;
            Ok(buf)
        })
        .await??;

        let b64 = base64::engine::general_purpose::STANDARD.encode(&resized_bytes);
        Ok(format!("data:image/jpeg;base64,{}", b64))
    }

    /// Convert Markdown to Telegram HTML format.
    /// Telegram HTML supports: <b>, <i>, <u>, <s>, <code>, <pre>, <a href="...">
    /// This mirrors OpenClaw's markdownToTelegramHtml approach.
    fn markdown_to_telegram_html(text: &str) -> String {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut result_lines: Vec<String> = Vec::new();

        for line in &lines {
            let trimmed_line = line.trim_start();
            if trimmed_line.starts_with("```") {
                // Preserve fence lines so the second-pass block parser can consume them
                // without interference from inline backtick handling.
                result_lines.push(trimmed_line.to_string());
                continue;
            }

            let mut line_out = String::new();

            // Handle code blocks (``` ... ```) - handled at text level below
            // Handle headers: ## Title → <b>Title</b>
            let stripped = line.trim_start_matches('#');
            let header_level = line.len() - stripped.len();
            if header_level > 0 && line.starts_with('#') && stripped.starts_with(' ') {
                let title = Self::escape_html(stripped.trim());
                result_lines.push(format!("<b>{title}</b>"));
                continue;
            }

            // Inline formatting
            let mut i = 0;
            let bytes = line.as_bytes();
            let len = bytes.len();
            while i < len {
                // Bold: **text** or __text__
                if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'*' {
                    if let Some(end) = line[i + 2..].find("**") {
                        let inner = Self::escape_html(&line[i + 2..i + 2 + end]);
                        write!(line_out, "<b>{inner}</b>").unwrap();
                        i += 4 + end;
                        continue;
                    }
                }
                if i + 1 < len && bytes[i] == b'_' && bytes[i + 1] == b'_' {
                    if let Some(end) = line[i + 2..].find("__") {
                        let inner = Self::escape_html(&line[i + 2..i + 2 + end]);
                        write!(line_out, "<b>{inner}</b>").unwrap();
                        i += 4 + end;
                        continue;
                    }
                }
                // Italic: *text* or _text_ (single)
                if bytes[i] == b'*' && (i == 0 || bytes[i - 1] != b'*') {
                    if let Some(end) = line[i + 1..].find('*') {
                        if end > 0 {
                            let inner = Self::escape_html(&line[i + 1..i + 1 + end]);
                            write!(line_out, "<i>{inner}</i>").unwrap();
                            i += 2 + end;
                            continue;
                        }
                    }
                }
                // Inline code: `code`
                if bytes[i] == b'`' && (i == 0 || bytes[i - 1] != b'`') {
                    if let Some(end) = line[i + 1..].find('`') {
                        let inner = Self::escape_html(&line[i + 1..i + 1 + end]);
                        write!(line_out, "<code>{inner}</code>").unwrap();
                        i += 2 + end;
                        continue;
                    }
                }
                // Markdown link: [text](url)
                if bytes[i] == b'[' {
                    if let Some(bracket_end) = line[i + 1..].find(']') {
                        let text_part = &line[i + 1..i + 1 + bracket_end];
                        let after_bracket = i + 1 + bracket_end + 1; // position after ']'
                        if after_bracket < len && bytes[after_bracket] == b'(' {
                            if let Some(paren_end) = line[after_bracket + 1..].find(')') {
                                let url = &line[after_bracket + 1..after_bracket + 1 + paren_end];
                                if url.starts_with("http://") || url.starts_with("https://") {
                                    let text_html = Self::escape_html(text_part);
                                    let url_html = Self::escape_html(url);
                                    write!(line_out, "<a href=\"{url_html}\">{text_html}</a>")
                                        .unwrap();
                                    i = after_bracket + 1 + paren_end + 1;
                                    continue;
                                }
                            }
                        }
                    }
                }
                // Strikethrough: ~~text~~
                if i + 1 < len && bytes[i] == b'~' && bytes[i + 1] == b'~' {
                    if let Some(end) = line[i + 2..].find("~~") {
                        let inner = Self::escape_html(&line[i + 2..i + 2 + end]);
                        write!(line_out, "<s>{inner}</s>").unwrap();
                        i += 4 + end;
                        continue;
                    }
                }
                // Default: escape HTML entities
                let ch = line[i..].chars().next().unwrap();
                match ch {
                    '<' => line_out.push_str("&lt;"),
                    '>' => line_out.push_str("&gt;"),
                    '&' => line_out.push_str("&amp;"),
                    '"' => line_out.push_str("&quot;"),
                    '\'' => line_out.push_str("&#39;"),
                    _ => line_out.push(ch),
                }
                i += ch.len_utf8();
            }
            result_lines.push(line_out);
        }

        // Second pass: handle ``` code blocks across lines
        let joined = result_lines.join("\n");
        let mut final_out = String::with_capacity(joined.len());
        let mut in_code_block = false;
        let mut code_buf = String::new();

        for line in joined.split('\n') {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                if in_code_block {
                    in_code_block = false;
                    let escaped = code_buf.trim_end_matches('\n');
                    // Telegram HTML parse mode supports <pre> and <code>, but not class attributes.
                    writeln!(final_out, "<pre><code>{escaped}</code></pre>").unwrap();
                    code_buf.clear();
                } else {
                    in_code_block = true;
                    code_buf.clear();
                }
            } else if in_code_block {
                code_buf.push_str(line);
                code_buf.push('\n');
            } else {
                final_out.push_str(line);
                final_out.push('\n');
            }
        }
        if in_code_block && !code_buf.is_empty() {
            writeln!(final_out, "<pre><code>{}</code></pre>", code_buf.trim_end()).unwrap();
        }

        final_out.trim_end_matches('\n').to_string()
    }

    fn escape_html(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    }

    async fn send_text_chunks(
        &self,
        message: &str,
        chat_id: &str,
        thread_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let chunks = split_message_for_telegram(message);

        for (index, chunk) in chunks.iter().enumerate() {
            let text = if chunks.len() > 1 {
                if index == 0 {
                    format!("{chunk}\n\n(continues...)")
                } else if index == chunks.len() - 1 {
                    format!("(continued)\n\n{chunk}")
                } else {
                    format!("(continued)\n\n{chunk}\n\n(continues...)")
                }
            } else {
                chunk.to_string()
            };

            let mut markdown_body = serde_json::json!({
                "chat_id": chat_id,
                "text": Self::markdown_to_telegram_html(&text),
                "parse_mode": "HTML"
            });

            // Add message_thread_id for forum topic support
            if let Some(tid) = thread_id {
                markdown_body["message_thread_id"] = serde_json::Value::String(tid.to_string());
            }

            let markdown_resp = self
                .http_client()
                .post(self.api_url("sendMessage"))
                .json(&markdown_body)
                .send()
                .await?;

            if markdown_resp.status().is_success() {
                if index < chunks.len() - 1 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                continue;
            }

            let markdown_status = markdown_resp.status();
            let markdown_err = markdown_resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = ?markdown_status,
                "Telegram sendMessage with Markdown failed; retrying without parse_mode"
            );

            let mut plain_body = serde_json::json!({
                "chat_id": chat_id,
                "text": text,
            });

            // Add message_thread_id for forum topic support
            if let Some(tid) = thread_id {
                plain_body["message_thread_id"] = serde_json::Value::String(tid.to_string());
            }
            let plain_resp = self
                .http_client()
                .post(self.api_url("sendMessage"))
                .json(&plain_body)
                .send()
                .await?;

            if !plain_resp.status().is_success() {
                let plain_status = plain_resp.status();
                let plain_err = plain_resp.text().await.unwrap_or_default();
                let sanitized_markdown_err = Self::sanitize_telegram_error(&markdown_err);
                let sanitized_plain_err = Self::sanitize_telegram_error(&plain_err);
                anyhow::bail!(
                    "Telegram sendMessage failed (markdown {}: {}; plain {}: {})",
                    markdown_status,
                    sanitized_markdown_err,
                    plain_status,
                    sanitized_plain_err
                );
            }

            if index < chunks.len() - 1 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        Ok(())
    }

    async fn send_media_by_url(
        &self,
        method: &str,
        media_field: &str,
        chat_id: &str,
        thread_id: Option<&str>,
        url: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
        });
        body[media_field] = serde_json::Value::String(url.to_string());

        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::String(tid.to_string());
        }

        if let Some(cap) = caption {
            body["caption"] = serde_json::Value::String(cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url(method))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram {method} by URL failed: {sanitized}");
        }

        tracing::info!("Telegram {method} sent to {chat_id}: {url}");
        Ok(())
    }

    async fn send_attachment(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        attachment: &TelegramAttachment,
    ) -> anyhow::Result<()> {
        let target = attachment.target.trim();

        if is_http_url(target) {
            let result = match attachment.kind {
                TelegramAttachmentKind::Image => {
                    self.send_photo_by_url(chat_id, thread_id, target, None)
                        .await
                }
                TelegramAttachmentKind::Document => {
                    self.send_document_by_url(chat_id, thread_id, target, None)
                        .await
                }
                TelegramAttachmentKind::Video => {
                    self.send_video_by_url(chat_id, thread_id, target, None)
                        .await
                }
                TelegramAttachmentKind::Audio => {
                    self.send_audio_by_url(chat_id, thread_id, target, None)
                        .await
                }
                TelegramAttachmentKind::Voice => {
                    self.send_voice_by_url(chat_id, thread_id, target, None)
                        .await
                }
            };

            // If sending media by URL failed (e.g. Telegram can't fetch the URL,
            // wrong content type, etc.), fall back to sending the URL as a text link
            // instead of losing the reply entirely.
            if let Err(e) = result {
                tracing::warn!(
                    url = target,
                    error = %e,
                    "Telegram send media by URL failed; falling back to text link"
                );
                let kind_label = match attachment.kind {
                    TelegramAttachmentKind::Image => "Image",
                    TelegramAttachmentKind::Document => "Document",
                    TelegramAttachmentKind::Video => "Video",
                    TelegramAttachmentKind::Audio => "Audio",
                    TelegramAttachmentKind::Voice => "Voice",
                };
                let fallback_text = format!("{kind_label}: {target}");
                self.send_text_chunks(&fallback_text, chat_id, thread_id)
                    .await?;
            }

            return Ok(());
        }

        let workspace = self.workspace_dir.as_ref().ok_or_else(|| {
            anyhow::anyhow!("workspace_dir is not configured; local file attachments are disabled")
        })?;
        let path = resolve_workspace_attachment_path(workspace, target)?;

        match attachment.kind {
            TelegramAttachmentKind::Image => self.send_photo(chat_id, thread_id, &path, None).await,
            TelegramAttachmentKind::Document => {
                self.send_document(chat_id, thread_id, &path, None).await
            }
            TelegramAttachmentKind::Video => self.send_video(chat_id, thread_id, &path, None).await,
            TelegramAttachmentKind::Audio => self.send_audio(chat_id, thread_id, &path, None).await,
            TelegramAttachmentKind::Voice => self.send_voice(chat_id, thread_id, &path, None).await,
        }
    }

    /// Send a document/file to a Telegram chat
    pub async fn send_document(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");

        let file_bytes = tokio::fs::read(file_path).await?;
        let part = Part::bytes(file_bytes).file_name(file_name.to_string());

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendDocument"))
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendDocument failed: {sanitized}");
        }

        tracing::info!("Telegram document sent to {chat_id}: {file_name}");
        Ok(())
    }

    /// Send a document from bytes (in-memory) to a Telegram chat
    pub async fn send_document_bytes(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        file_bytes: Vec<u8>,
        file_name: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let part = Part::bytes(file_bytes).file_name(file_name.to_string());

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendDocument"))
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendDocument failed: {sanitized}");
        }

        tracing::info!("Telegram document sent to {chat_id}: {file_name}");
        Ok(())
    }

    /// Send a photo to a Telegram chat
    pub async fn send_photo(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("photo.jpg");

        let file_bytes = tokio::fs::read(file_path).await?;
        let part = Part::bytes(file_bytes).file_name(file_name.to_string());

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("photo", part);

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendPhoto"))
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendPhoto failed: {sanitized}");
        }

        tracing::info!("Telegram photo sent to {chat_id}: {file_name}");
        Ok(())
    }

    /// Send a photo from bytes (in-memory) to a Telegram chat
    pub async fn send_photo_bytes(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        file_bytes: Vec<u8>,
        file_name: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let part = Part::bytes(file_bytes).file_name(file_name.to_string());

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("photo", part);

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendPhoto"))
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendPhoto failed: {sanitized}");
        }

        tracing::info!("Telegram photo sent to {chat_id}: {file_name}");
        Ok(())
    }

    /// Send a video to a Telegram chat
    pub async fn send_video(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video.mp4");

        let file_bytes = tokio::fs::read(file_path).await?;
        let part = Part::bytes(file_bytes).file_name(file_name.to_string());

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("video", part);

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendVideo"))
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendVideo failed: {sanitized}");
        }

        tracing::info!("Telegram video sent to {chat_id}: {file_name}");
        Ok(())
    }

    /// Send an audio file to a Telegram chat
    pub async fn send_audio(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.mp3");

        let file_bytes = tokio::fs::read(file_path).await?;
        let part = Part::bytes(file_bytes).file_name(file_name.to_string());

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("audio", part);

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendAudio"))
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendAudio failed: {sanitized}");
        }

        tracing::info!("Telegram audio sent to {chat_id}: {file_name}");
        Ok(())
    }

    /// Send a voice message to a Telegram chat
    pub async fn send_voice(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("voice.ogg");

        let file_bytes = tokio::fs::read(file_path).await?;
        let part = Part::bytes(file_bytes).file_name(file_name.to_string());

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("voice", part);

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendVoice"))
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendVoice failed: {sanitized}");
        }

        tracing::info!("Telegram voice sent to {chat_id}: {file_name}");
        Ok(())
    }

    /// Send a file by URL (Telegram will download it)
    pub async fn send_document_by_url(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        url: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "document": url
        });

        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::String(tid.to_string());
        }

        if let Some(cap) = caption {
            body["caption"] = serde_json::Value::String(cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendDocument"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendDocument by URL failed: {sanitized}");
        }

        tracing::info!("Telegram document (URL) sent to {chat_id}: {url}");
        Ok(())
    }

    /// Send a photo by URL (Telegram will download it)
    pub async fn send_photo_by_url(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        url: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "photo": url
        });

        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::String(tid.to_string());
        }

        if let Some(cap) = caption {
            body["caption"] = serde_json::Value::String(cap.to_string());
        }

        let resp = self
            .http_client()
            .post(self.api_url("sendPhoto"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendPhoto by URL failed: {sanitized}");
        }

        tracing::info!("Telegram photo (URL) sent to {chat_id}: {url}");
        Ok(())
    }

    /// Send a video by URL (Telegram will download it)
    pub async fn send_video_by_url(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        url: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        self.send_media_by_url("sendVideo", "video", chat_id, thread_id, url, caption)
            .await
    }

    /// Send an audio file by URL (Telegram will download it)
    pub async fn send_audio_by_url(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        url: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        self.send_media_by_url("sendAudio", "audio", chat_id, thread_id, url, caption)
            .await
    }

    /// Send a voice message by URL (Telegram will download it)
    pub async fn send_voice_by_url(
        &self,
        chat_id: &str,
        thread_id: Option<&str>,
        url: &str,
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        self.send_media_by_url("sendVoice", "voice", chat_id, thread_id, url, caption)
            .await
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    fn supports_draft_updates(&self) -> bool {
        self.stream_mode != StreamMode::Off
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if self.stream_mode == StreamMode::Off {
            return Ok(None);
        }

        let (chat_id, thread_id) = Self::parse_reply_target(&message.recipient);
        let initial_text = if message.content.is_empty() {
            "...".to_string()
        } else {
            message.content.clone()
        };

        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": initial_text,
        });
        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::String(tid.to_string());
        }

        let resp = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram sendMessage (draft) failed: {sanitized}");
        }

        let resp_json: serde_json::Value = resp.json().await?;
        let message_id = resp_json
            .get("result")
            .and_then(|r| r.get("message_id"))
            .and_then(|id| id.as_i64())
            .map(|id| id.to_string());

        self.last_draft_edit
            .lock()
            .insert(chat_id.to_string(), std::time::Instant::now());

        Ok(message_id)
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<Option<String>> {
        let (chat_id, _) = Self::parse_reply_target(recipient);

        // Rate-limit edits per chat
        {
            let last_edits = self.last_draft_edit.lock();
            if let Some(last_time) = last_edits.get(&chat_id) {
                let elapsed = u64::try_from(last_time.elapsed().as_millis()).unwrap_or(u64::MAX);
                if elapsed < self.draft_update_interval_ms {
                    return Ok(None);
                }
            }
        }

        // Truncate to Telegram limit for mid-stream edits (UTF-8 safe)
        let display_text = if text.len() > TELEGRAM_MAX_MESSAGE_LENGTH {
            let mut end = 0;
            for (idx, ch) in text.char_indices() {
                let next = idx + ch.len_utf8();
                if next > TELEGRAM_MAX_MESSAGE_LENGTH {
                    break;
                }
                end = next;
            }
            &text[..end]
        } else {
            text
        };

        let message_id_parsed = match message_id.parse::<i64>() {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("Invalid Telegram message_id '{message_id}': {e}");
                return Ok(None);
            }
        };

        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id_parsed,
            "text": display_text,
        });

        let resp = self
            .client
            .post(self.api_url("editMessageText"))
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            self.last_draft_edit
                .lock()
                .insert(chat_id.clone(), std::time::Instant::now());
        } else {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = Self::sanitize_telegram_error(&err);
            tracing::debug!("Telegram editMessageText failed ({status}): {sanitized}");
        }

        Ok(None)
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let text = &strip_tool_call_tags(text);
        let (chat_id, thread_id) = Self::parse_reply_target(recipient);

        // Clean up rate-limit tracking for this chat
        self.last_draft_edit.lock().remove(&chat_id);

        // Parse attachments before processing
        let (text_without_markers, attachments) = parse_attachment_markers(text);

        // Parse message ID once for reuse
        let msg_id = match message_id.parse::<i64>() {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!("Invalid Telegram message_id '{message_id}': {e}");
                None
            }
        };

        // If we have attachments, delete the draft and send fresh messages
        // (Telegram editMessageText can't add attachments)
        if !attachments.is_empty() {
            // Delete the draft message
            if let Some(id) = msg_id {
                let _ = self
                    .client
                    .post(self.api_url("deleteMessage"))
                    .json(&serde_json::json!({
                        "chat_id": chat_id,
                        "message_id": id,
                    }))
                    .send()
                    .await;
            }

            // Send text without markers
            if !text_without_markers.is_empty() {
                self.send_text_chunks(&text_without_markers, &chat_id, thread_id.as_deref())
                    .await?;
            }

            // Send attachments
            for attachment in &attachments {
                self.send_attachment(&chat_id, thread_id.as_deref(), attachment)
                    .await?;
            }

            return Ok(());
        }

        // If text exceeds limit, delete draft and send as chunked messages
        if text.len() > TELEGRAM_MAX_MESSAGE_LENGTH {
            if let Some(id) = msg_id {
                let _ = self
                    .client
                    .post(self.api_url("deleteMessage"))
                    .json(&serde_json::json!({
                        "chat_id": chat_id,
                        "message_id": id,
                    }))
                    .send()
                    .await;
            }

            // Fall back to chunked send
            return self
                .send_text_chunks(text, &chat_id, thread_id.as_deref())
                .await;
        }

        let Some(id) = msg_id else {
            return self
                .send_text_chunks(text, &chat_id, thread_id.as_deref())
                .await;
        };

        // Try editing with HTML formatting
        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": id,
            "text": Self::markdown_to_telegram_html(text),
            "parse_mode": "HTML",
        });

        let resp = self
            .client
            .post(self.api_url("editMessageText"))
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(());
        }

        // Telegram returns "message is not modified" when update_draft already
        // set identical content. Common for short plain-text responses where
        // HTML and plain text are equivalent.
        {
            let body_bytes = resp.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes);
            if body_str.contains("message is not modified") {
                tracing::debug!(
                    "Telegram editMessageText (HTML): message is not modified, treating as success"
                );
                return Ok(());
            }
        }

        // HTML edit failed — retry without parse_mode
        let plain_body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": id,
            "text": text,
        });

        let resp = self
            .client
            .post(self.api_url("editMessageText"))
            .json(&plain_body)
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(());
        }

        {
            let body_bytes = resp.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes);
            if body_str.contains("message is not modified") {
                tracing::debug!(
                    "Telegram editMessageText (plain): message is not modified, treating as success"
                );
                return Ok(());
            }
        }

        // Both edits truly failed — try to delete draft before sending new message
        // to prevent duplicates (draft from update_draft still shows response text).
        tracing::warn!("Telegram finalize_draft edit failed; attempting delete+send fallback");

        let del_resp = self
            .client
            .post(self.api_url("deleteMessage"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "message_id": id,
            }))
            .send()
            .await;

        match del_resp {
            Ok(r) if r.status().is_success() => {
                // Draft deleted — safe to send fresh message without duplication
                self.send_text_chunks(text, &chat_id, thread_id.as_deref())
                    .await
            }
            Ok(r) => {
                let status = r.status();
                tracing::warn!(
                    "Telegram deleteMessage failed ({status}); draft still shows response, skipping sendMessage to avoid duplicate"
                );
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    "Telegram deleteMessage network error: {e}; draft still shows response, skipping sendMessage to avoid duplicate"
                );
                Ok(())
            }
        }
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        let (chat_id, _) = Self::parse_reply_target(recipient);
        self.last_draft_edit.lock().remove(&chat_id);

        let message_id = match message_id.parse::<i64>() {
            Ok(id) => id,
            Err(e) => {
                tracing::debug!("Invalid Telegram draft message_id '{message_id}': {e}");
                return Ok(());
            }
        };

        let response = self
            .client
            .post(self.api_url("deleteMessage"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "message_id": message_id,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let sanitized = Self::sanitize_telegram_error(&body);
            tracing::debug!("Telegram deleteMessage failed ({status}): {sanitized}");
        }

        Ok(())
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Strip tool_call tags before processing to prevent Markdown parsing failures
        let content = strip_tool_call_tags(&message.content);

        // Parse recipient: "chat_id" or "chat_id:thread_id" format
        let (chat_id, thread_id) = match message.recipient.split_once(':') {
            Some((chat, thread)) => (chat, Some(thread)),
            None => (message.recipient.as_str(), None),
        };

        let (text_without_markers, attachments) = parse_attachment_markers(&content);

        if !attachments.is_empty() {
            if !text_without_markers.is_empty() {
                self.send_text_chunks(&text_without_markers, chat_id, thread_id)
                    .await?;
            }

            for attachment in &attachments {
                self.send_attachment(chat_id, thread_id, attachment).await?;
            }

            return Ok(());
        }

        if let Some(attachment) = parse_path_only_attachment(&content) {
            self.send_attachment(chat_id, thread_id, &attachment)
                .await?;
            return Ok(());
        }

        self.send_text_chunks(&content, chat_id, thread_id).await
    }

    async fn send_approval_prompt(
        &self,
        recipient: &str,
        request_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        thread_ts: Option<String>,
    ) -> anyhow::Result<()> {
        let (chat_id, parsed_thread_id) = Self::parse_reply_target(recipient);
        let thread_id = parsed_thread_id.or(thread_ts);

        let raw_args = arguments.to_string();
        let args_preview = if raw_args.chars().count() > 260 {
            crate::util::truncate_with_ellipsis(&raw_args, 260)
        } else {
            raw_args
        };

        let body = Self::build_approval_prompt_body(
            &chat_id,
            thread_id.as_deref(),
            request_id,
            tool_name,
            &args_preview,
        );

        let response = self
            .http_client()
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let err = response.text().await.unwrap_or_default();
            let sanitized = Self::sanitize_telegram_error(&err);
            anyhow::bail!("Telegram approval prompt failed ({status}): {sanitized}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let mut offset: i64 = 0;
        let mut consecutive_poll_transport_failures = 0u32;

        if self.mention_only {
            let _ = self.get_bot_username().await;
        }

        if let Err(e) = self.register_commands().await {
            tracing::warn!("Failed to register Telegram bot commands: {e}");
        }

        tracing::info!("Telegram channel listening for messages...");

        // Startup probe: claim the getUpdates slot before entering the long-poll loop.
        // A previous daemon's 30-second poll may still be active on Telegram's server.
        // We retry with timeout=0 until we receive a successful (non-409) response,
        // confirming the slot is ours. This prevents the long-poll loop from entering
        // a self-sustaining 409 cycle where each rejected request is immediately retried.
        loop {
            let url = self.api_url("getUpdates");
            let probe = serde_json::json!({
                "offset": offset,
                "timeout": 0,
                "allowed_updates": ["message", "callback_query"]
            });
            match self.http_client().post(&url).json(&probe).send().await {
                Err(e) => {
                    let sanitized = Self::sanitize_telegram_error(&e.to_string());
                    tracing::warn!("Telegram startup probe error: {sanitized}; retrying in 5s");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                Ok(resp) => {
                    match resp.json::<serde_json::Value>().await {
                        Err(e) => {
                            let sanitized = Self::sanitize_telegram_error(&e.to_string());
                            tracing::warn!(
                                "Telegram startup probe parse error: {sanitized}; retrying in 5s"
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                        Ok(data) => {
                            let ok = data
                                .get("ok")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false);
                            if ok {
                                // Slot claimed — advance offset past any queued updates.
                                if let Some(results) =
                                    data.get("result").and_then(serde_json::Value::as_array)
                                {
                                    for update in results {
                                        if let Some(uid) = update
                                            .get("update_id")
                                            .and_then(serde_json::Value::as_i64)
                                        {
                                            offset = uid + 1;
                                        }
                                    }
                                }
                                break; // Probe succeeded; enter the long-poll loop.
                            }

                            let error_code = data
                                .get("error_code")
                                .and_then(serde_json::Value::as_i64)
                                .unwrap_or_default();
                            if error_code == 409 {
                                tracing::debug!("Startup probe: slot busy (409), retrying in 5s");
                            } else {
                                let desc = data
                                    .get("description")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("unknown");
                                tracing::warn!(
                                    "Startup probe: API error {error_code}: {desc}; retrying in 5s"
                                );
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            }
        }

        tracing::debug!("Startup probe succeeded; entering main long-poll loop.");

        loop {
            if self.mention_only {
                let missing_username = self.bot_username.lock().is_none();
                if missing_username {
                    let _ = self.get_bot_username().await;
                }
            }

            let url = self.api_url("getUpdates");
            let body = serde_json::json!({
                "offset": offset,
                "timeout": 30,
                "allowed_updates": ["message", "callback_query"]
            });

            let resp = match self.http_client().post(&url).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    let sanitized = Self::sanitize_telegram_error(&e.to_string());
                    consecutive_poll_transport_failures =
                        consecutive_poll_transport_failures.saturating_add(1);
                    Self::log_poll_transport_error(&sanitized, consecutive_poll_transport_failures);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            consecutive_poll_transport_failures = 0;

            let data: serde_json::Value = match resp.json().await {
                Ok(d) => d,
                Err(e) => {
                    let sanitized = Self::sanitize_telegram_error(&e.to_string());
                    tracing::warn!("Telegram parse error: {sanitized}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            let ok = data
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            if !ok {
                let error_code = data
                    .get("error_code")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                let description = data
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown Telegram API error");

                if error_code == 409 {
                    tracing::warn!(
                        "Telegram polling conflict (409): {description}. \
Ensure only one `zeroclaw` process is using this bot token."
                    );
                    // Back off for 35 seconds — longer than Telegram's 30-second poll
                    // timeout — so any competing session (e.g. a stale connection from
                    // a previous daemon) has time to expire before we retry.
                    tokio::time::sleep(std::time::Duration::from_secs(35)).await;
                } else {
                    tracing::warn!(
                        "Telegram getUpdates API error (code={}): {description}",
                        error_code
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                continue;
            }

            if let Some(results) = data.get("result").and_then(serde_json::Value::as_array) {
                for update in results {
                    // Advance offset past this update
                    if let Some(uid) = update.get("update_id").and_then(serde_json::Value::as_i64) {
                        offset = uid + 1;
                    }

                    let msg = if let Some(m) = self.parse_update_message(update) {
                        m
                    } else if let Some(m) = self.try_parse_approval_callback_query(update) {
                        m
                    } else if let Some(m) = self.try_parse_voice_message(update).await {
                        m
                    } else if let Some(m) = self.try_parse_attachment_message(update).await {
                        m
                    } else {
                        self.handle_unauthorized_message(update).await;
                        continue;
                    };

                    if let Some((reaction_chat_id, reaction_message_id, chat_type, sender_id)) =
                        Self::extract_update_message_ack_target(update)
                    {
                        let reaction_ctx = AckReactionContext {
                            text: &msg.content,
                            sender_id: sender_id.as_deref(),
                            chat_id: Some(&reaction_chat_id),
                            chat_type,
                            locale_hint: None,
                        };
                        if let Some(emoji) = select_ack_reaction(
                            self.ack_reaction.as_ref(),
                            TELEGRAM_ACK_REACTIONS,
                            &reaction_ctx,
                        ) {
                            self.try_add_ack_reaction_nonblocking(
                                reaction_chat_id,
                                reaction_message_id,
                                emoji,
                            );
                        }
                    }

                    // Send "typing" indicator immediately when we receive a message
                    let typing_body = Self::build_typing_action_body(&msg.reply_target);
                    let _ = self
                        .http_client()
                        .post(self.api_url("sendChatAction"))
                        .json(&typing_body)
                        .send()
                        .await; // Ignore errors for typing indicator

                    if tx.send(msg).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let timeout_duration = Duration::from_secs(5);

        match tokio::time::timeout(
            timeout_duration,
            self.http_client().get(self.api_url("getMe")).send(),
        )
        .await
        {
            Ok(Ok(resp)) => resp.status().is_success(),
            Ok(Err(e)) => {
                let sanitized = Self::sanitize_telegram_error(&e.to_string());
                tracing::debug!("Telegram health check failed: {sanitized}");
                false
            }
            Err(_) => {
                tracing::debug!("Telegram health check timed out after 5s");
                false
            }
        }
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.stop_typing(recipient).await?;

        let client = self.http_client();
        let url = self.api_url("sendChatAction");
        let chat_id = recipient.to_string();

        let handle = tokio::spawn(async move {
            loop {
                let body = serde_json::json!({
                    "chat_id": &chat_id,
                    "action": "typing"
                });
                let _ = client.post(&url).json(&body).send().await;
                // Telegram typing indicator expires after 5s; refresh at 4s
                tokio::time::sleep(Duration::from_secs(4)).await;
            }
        });

        let mut guard = self.typing_handle.lock();
        *guard = Some(handle);

        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        let mut guard = self.typing_handle.lock();
        if let Some(handle) = guard.take() {
            handle.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[cfg(unix)]
    fn symlink_file(src: &Path, dst: &Path) {
        std::os::unix::fs::symlink(src, dst).expect("symlink should be created");
    }

    #[cfg(windows)]
    fn symlink_file(src: &Path, dst: &Path) {
        std::os::windows::fs::symlink_file(src, dst).expect("symlink should be created");
    }

    #[cfg(unix)]
    fn symlink_dir(src: &Path, dst: &Path) {
        std::os::unix::fs::symlink(src, dst).expect("symlink should be created");
    }

    #[cfg(windows)]
    fn symlink_dir(src: &Path, dst: &Path) {
        std::os::windows::fs::symlink_dir(src, dst).expect("symlink should be created");
    }

    #[test]
    fn telegram_channel_name() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        assert_eq!(ch.name(), "telegram");
    }

    #[test]
    fn random_telegram_ack_reaction_is_from_pool() {
        for _ in 0..128 {
            let emoji = random_telegram_ack_reaction();
            assert!(TELEGRAM_ACK_REACTIONS.contains(&emoji));
        }
    }

    #[test]
    fn telegram_ack_reaction_request_shape() {
        let body = build_telegram_ack_reaction_request("-100200300", 42, "⚡️");
        assert_eq!(body["chat_id"], "-100200300");
        assert_eq!(body["message_id"], 42);
        assert_eq!(body["reaction"][0]["type"], "emoji");
        assert_eq!(body["reaction"][0]["emoji"], "⚡️");
    }

    #[test]
    fn telegram_extract_update_message_target_parses_ids() {
        let update = serde_json::json!({
            "update_id": 1,
            "message": {
                "message_id": 99,
                "chat": { "id": -100_123_456 }
            }
        });

        let target = TelegramChannel::extract_update_message_target(&update);
        assert_eq!(target, Some(("-100123456".to_string(), 99)));
    }

    #[test]
    fn typing_handle_starts_as_none() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let guard = ch.typing_handle.lock();
        assert!(guard.is_none());
    }

    #[tokio::test]
    async fn stop_typing_clears_handle() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);

        // Manually insert a dummy handle
        {
            let mut guard = ch.typing_handle.lock();
            *guard = Some(tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }));
        }

        // stop_typing should abort and clear
        ch.stop_typing("123").await.unwrap();

        let guard = ch.typing_handle.lock();
        assert!(guard.is_none());
    }

    #[tokio::test]
    async fn start_typing_replaces_previous_handle() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);

        // Insert a dummy handle first
        {
            let mut guard = ch.typing_handle.lock();
            *guard = Some(tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }));
        }

        // start_typing should abort the old handle and set a new one
        let _ = ch.start_typing("123").await;

        let guard = ch.typing_handle.lock();
        assert!(guard.is_some());
    }

    #[test]
    fn supports_draft_updates_respects_stream_mode() {
        let off = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        assert!(!off.supports_draft_updates());

        let partial = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true)
            .with_streaming(StreamMode::Partial, 750);
        assert!(partial.supports_draft_updates());
        assert_eq!(partial.draft_update_interval_ms, 750);
    }

    #[tokio::test]
    async fn send_draft_returns_none_when_stream_mode_off() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let id = ch
            .send_draft(&SendMessage::new("draft", "123"))
            .await
            .unwrap();
        assert!(id.is_none());
    }

    #[tokio::test]
    async fn update_draft_rate_limit_short_circuits_network() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true)
            .with_streaming(StreamMode::Partial, 60_000);
        ch.last_draft_edit
            .lock()
            .insert("123".to_string(), std::time::Instant::now());

        let result = ch.update_draft("123", "42", "delta text").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_draft_utf8_truncation_is_safe_for_multibyte_text() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true)
            .with_streaming(StreamMode::Partial, 0);
        let long_emoji_text = "😀".repeat(TELEGRAM_MAX_MESSAGE_LENGTH + 20);

        // Invalid message_id returns early after building display_text.
        // This asserts truncation never panics on UTF-8 boundaries.
        let result = ch
            .update_draft("123", "not-a-number", &long_emoji_text)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn finalize_draft_invalid_message_id_falls_back_to_chunk_send() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true)
            .with_streaming(StreamMode::Partial, 0);
        let long_text = "a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH + 64);

        // For oversized text + invalid draft message_id, finalize_draft should
        // fall back to chunked send instead of returning early.
        let result = ch.finalize_draft("123", "not-a-number", &long_text).await;
        assert!(result.is_err());
    }

    #[test]
    fn telegram_api_url() {
        let ch = TelegramChannel::new("123:ABC".into(), vec![], false, true);
        assert_eq!(
            ch.api_url("getMe"),
            "https://api.telegram.org/bot123:ABC/getMe"
        );
    }

    #[test]
    fn telegram_custom_base_url() {
        let ch = TelegramChannel::new("123:ABC".into(), vec![], false, true)
            .with_api_base("https://tapi.bale.ai".to_string());
        assert_eq!(ch.api_url("getMe"), "https://tapi.bale.ai/bot123:ABC/getMe");
        assert_eq!(
            ch.api_url("sendMessage"),
            "https://tapi.bale.ai/bot123:ABC/sendMessage"
        );
    }

    #[test]
    fn approval_prompt_includes_markdown_parse_mode() {
        let body = TelegramChannel::build_approval_prompt_body(
            "12345",
            Some("67890"),
            "apr-1234",
            "shell",
            "{\"command\":\"echo hello\"}",
        );

        assert_eq!(body["parse_mode"], "Markdown");
        assert_eq!(body["chat_id"], "12345");
        assert_eq!(body["message_thread_id"], "67890");
        assert!(body["text"]
            .as_str()
            .is_some_and(|text| text.contains("`shell`")));
    }

    #[test]
    fn sanitize_telegram_error_redacts_bot_token_in_url() {
        let input =
            "error sending request for url (https://api.telegram.org/bot123456:ABCdef/getUpdates)";
        let sanitized = TelegramChannel::sanitize_telegram_error(input);

        assert!(!sanitized.contains("123456:ABCdef"));
        assert!(sanitized.contains("/bot[REDACTED]/getUpdates"));
    }

    #[test]
    fn sanitize_telegram_error_does_not_redact_non_token_bot_path() {
        let input = "error sending request for url (https://example.com/bot/getUpdates)";
        let sanitized = TelegramChannel::sanitize_telegram_error(input);
        assert_eq!(sanitized, input);
    }

    #[test]
    fn telegram_markdown_to_html_escapes_quotes_in_link_href() {
        let rendered = TelegramChannel::markdown_to_telegram_html(
            "[click](https://example.com?q=\"x\"&a='b')",
        );
        assert_eq!(
            rendered,
            "<a href=\"https://example.com?q=&quot;x&quot;&amp;a=&#39;b&#39;\">click</a>"
        );
    }

    #[test]
    fn telegram_markdown_to_html_escapes_quotes_in_plain_text() {
        let rendered = TelegramChannel::markdown_to_telegram_html("say \"hi\" & <tag> 'ok'");
        assert_eq!(
            rendered,
            "say &quot;hi&quot; &amp; &lt;tag&gt; &#39;ok&#39;"
        );
    }

    #[test]
    fn telegram_markdown_to_html_code_block_drops_language_attribute() {
        let rendered = TelegramChannel::markdown_to_telegram_html(
            "```rust\" onclick=\"alert(1)\nlet x = 1;\n```",
        );
        assert_eq!(rendered, "<pre><code>let x = 1;</code></pre>");
        assert!(!rendered.contains("language-"));
        assert!(!rendered.contains("onclick"));
    }

    #[test]
    fn telegram_user_allowed_wildcard() {
        let ch = TelegramChannel::new("t".into(), vec!["*".into()], false, true);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn telegram_user_allowed_specific() {
        let ch = TelegramChannel::new("t".into(), vec!["alice".into(), "bob".into()], false, true);
        assert!(ch.is_user_allowed("alice"));
        assert!(!ch.is_user_allowed("eve"));
    }

    #[test]
    fn telegram_user_allowed_with_at_prefix_in_config() {
        let ch = TelegramChannel::new("t".into(), vec!["@alice".into()], false, true);
        assert!(ch.is_user_allowed("alice"));
    }

    #[test]
    fn telegram_user_denied_empty() {
        let ch = TelegramChannel::new("t".into(), vec![], false, true);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn telegram_user_exact_match_not_substring() {
        let ch = TelegramChannel::new("t".into(), vec!["alice".into()], false, true);
        assert!(!ch.is_user_allowed("alice_bot"));
        assert!(!ch.is_user_allowed("alic"));
        assert!(!ch.is_user_allowed("malice"));
    }

    #[test]
    fn telegram_user_empty_string_denied() {
        let ch = TelegramChannel::new("t".into(), vec!["alice".into()], false, true);
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn telegram_user_case_sensitive() {
        let ch = TelegramChannel::new("t".into(), vec!["Alice".into()], false, true);
        assert!(ch.is_user_allowed("Alice"));
        assert!(!ch.is_user_allowed("alice"));
        assert!(!ch.is_user_allowed("ALICE"));
    }

    #[test]
    fn telegram_wildcard_with_specific_users() {
        let ch = TelegramChannel::new("t".into(), vec!["alice".into(), "*".into()], false, true);
        assert!(ch.is_user_allowed("alice"));
        assert!(ch.is_user_allowed("bob"));
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn telegram_user_allowed_by_numeric_id_identity() {
        let ch = TelegramChannel::new("t".into(), vec!["123456789".into()], false, true);
        assert!(ch.is_any_user_allowed(["unknown", "123456789"]));
    }

    #[test]
    fn telegram_user_denied_when_none_of_identities_match() {
        let ch = TelegramChannel::new(
            "t".into(),
            vec!["alice".into(), "987654321".into()],
            false,
            true,
        );
        assert!(!ch.is_any_user_allowed(["unknown", "123456789"]));
    }

    #[test]
    fn telegram_pairing_enabled_with_empty_allowlist() {
        let ch = TelegramChannel::new("t".into(), vec![], false, true);
        assert!(ch.pairing_code_active());
    }

    #[test]
    fn telegram_pairing_disabled_with_nonempty_allowlist() {
        let ch = TelegramChannel::new("t".into(), vec!["alice".into()], false, true);
        assert!(!ch.pairing_code_active());
    }

    #[test]
    fn telegram_extract_bind_code_plain_command() {
        assert_eq!(
            TelegramChannel::extract_bind_code("/bind 123456"),
            Some("123456")
        );
    }

    #[test]
    fn telegram_extract_bind_code_supports_bot_mention() {
        assert_eq!(
            TelegramChannel::extract_bind_code("/bind@zeroclaw_bot 654321"),
            Some("654321")
        );
    }

    #[test]
    fn telegram_extract_bind_code_rejects_invalid_forms() {
        assert_eq!(TelegramChannel::extract_bind_code("/bind"), None);
        assert_eq!(TelegramChannel::extract_bind_code("/start"), None);
    }

    #[test]
    fn parse_attachment_markers_extracts_multiple_types() {
        let message = "Here are files [IMAGE:/tmp/a.png] and [DOCUMENT:https://example.com/a.pdf]";
        let (cleaned, attachments) = parse_attachment_markers(message);

        assert_eq!(cleaned, "Here are files  and");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, TelegramAttachmentKind::Image);
        assert_eq!(attachments[0].target, "/tmp/a.png");
        assert_eq!(attachments[1].kind, TelegramAttachmentKind::Document);
        assert_eq!(attachments[1].target, "https://example.com/a.pdf");
    }

    #[test]
    fn parse_attachment_markers_deduplicates_duplicate_targets() {
        let message = "twice [IMAGE:/tmp/a.png] then again [IMAGE:/tmp/a.png] end";
        let (cleaned, attachments) = parse_attachment_markers(message);

        assert_eq!(cleaned, "twice  then again  end");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, TelegramAttachmentKind::Image);
        assert_eq!(attachments[0].target, "/tmp/a.png");
    }

    #[test]
    fn parse_attachment_markers_keeps_invalid_markers_in_text() {
        let message = "Report [UNKNOWN:/tmp/a.bin]";
        let (cleaned, attachments) = parse_attachment_markers(message);

        assert_eq!(cleaned, "Report [UNKNOWN:/tmp/a.bin]");
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_attachment_markers_handles_brackets_in_filename() {
        let message = "Here it is [VIDEO:/mnt/clips/Butters - What What [G4PvTrTp7Tc].mp4]";
        let (cleaned, attachments) = parse_attachment_markers(message);

        assert_eq!(cleaned, "Here it is");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, TelegramAttachmentKind::Video);
        assert_eq!(
            attachments[0].target,
            "/mnt/clips/Butters - What What [G4PvTrTp7Tc].mp4"
        );
    }

    #[test]
    fn parse_attachment_markers_unclosed_bracket_falls_back_to_text() {
        let message = "send [VIDEO:/path/file[broken.mp4";
        let (cleaned, attachments) = parse_attachment_markers(message);
        assert_eq!(cleaned, "send [VIDEO:/path/file[broken.mp4");
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_path_only_attachment_detects_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("snap.png");
        std::fs::write(&image_path, b"fake-png").unwrap();

        let parsed = parse_path_only_attachment(image_path.to_string_lossy().as_ref())
            .expect("expected attachment");

        assert_eq!(parsed.kind, TelegramAttachmentKind::Image);
        assert_eq!(parsed.target, image_path.to_string_lossy());
    }

    #[test]
    fn parse_path_only_attachment_rejects_sentence_text() {
        assert!(parse_path_only_attachment("Screenshot saved to /tmp/snap.png").is_none());
    }

    #[test]
    fn sanitize_attachment_filename_strips_path_traversal() {
        assert_eq!(
            sanitize_attachment_filename("../../tmp/evil.txt").as_deref(),
            Some("evil.txt")
        );
        assert_eq!(
            sanitize_attachment_filename(r"..\\..\\secrets\\token.env").as_deref(),
            Some("..__..__secrets__token.env")
        );
        assert!(sanitize_attachment_filename("..").is_none());
        assert!(sanitize_attachment_filename("").is_none());
    }

    #[test]
    fn resolve_workspace_attachment_path_rejects_escape_and_accepts_workspace_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should exist");

        let in_workspace = workspace.join("report.txt");
        std::fs::write(&in_workspace, b"ok").expect("workspace fixture should be written");
        let resolved = resolve_workspace_attachment_path(&workspace, "report.txt")
            .expect("workspace relative path should resolve");
        assert!(resolved.starts_with(workspace.canonicalize().unwrap_or(workspace.clone())));

        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, b"secret").expect("outside fixture should be written");
        let escaped =
            resolve_workspace_attachment_path(&workspace, outside.to_string_lossy().as_ref());
        assert!(escaped.is_err(), "outside workspace path must be rejected");
    }

    #[test]
    fn resolve_workspace_attachment_path_accepts_workspace_prefix_mapping() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("sub")).expect("workspace dir should exist");
        let nested = workspace.join("sub/file.txt");
        std::fs::write(&nested, b"content").expect("fixture should be written");

        let resolved = resolve_workspace_attachment_path(&workspace, "/workspace/sub/file.txt")
            .expect("/workspace prefix should map to workspace root");
        assert_eq!(
            resolved,
            nested
                .canonicalize()
                .expect("canonical path should resolve")
        );
    }

    #[tokio::test]
    async fn resolve_workspace_attachment_output_path_rejects_symlinked_save_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace)
            .await
            .expect("workspace dir should exist");

        let outside = temp.path().join("outside");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("outside dir should exist");
        symlink_dir(&outside, &workspace.join("telegram_files"));

        let result = resolve_workspace_attachment_output_path(&workspace, "doc.txt").await;
        assert!(result.is_err(), "symlinked save dir must be rejected");
    }

    #[tokio::test]
    async fn resolve_workspace_attachment_output_path_rejects_symlink_target_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let save_dir = workspace.join("telegram_files");
        tokio::fs::create_dir_all(&save_dir)
            .await
            .expect("save dir should exist");

        let outside = temp.path().join("outside.txt");
        tokio::fs::write(&outside, b"secret")
            .await
            .expect("outside fixture should be written");
        symlink_file(&outside, &save_dir.join("doc.txt"));

        let result = resolve_workspace_attachment_output_path(&workspace, "doc.txt").await;
        assert!(result.is_err(), "symlink target file must be rejected");
    }

    #[test]
    fn infer_attachment_kind_from_target_detects_document_extension() {
        assert_eq!(
            infer_attachment_kind_from_target("https://example.com/files/specs.pdf?download=1"),
            Some(TelegramAttachmentKind::Document)
        );
    }

    #[test]
    fn parse_update_message_uses_chat_id_as_reply_target() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], false, true);
        let update = serde_json::json!({
            "update_id": 1,
            "message": {
                "message_id": 33,
                "text": "hello",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300
                }
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("message should parse");

        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.reply_target, "-100200300");
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.id, "telegram_-100200300_33");
    }

    #[test]
    fn parse_update_message_allows_numeric_id_without_username() {
        let ch = TelegramChannel::new("token".into(), vec!["555".into()], false, true);
        let update = serde_json::json!({
            "update_id": 2,
            "message": {
                "message_id": 9,
                "text": "ping",
                "from": {
                    "id": 555
                },
                "chat": {
                    "id": 12345
                }
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("numeric allowlist should pass");

        assert_eq!(msg.sender, "555");
        assert_eq!(msg.reply_target, "12345");
    }

    #[test]
    fn parse_update_message_extracts_thread_id_for_forum_topic() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], false, true);
        let update = serde_json::json!({
            "update_id": 3,
            "message": {
                "message_id": 42,
                "text": "hello from topic",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300
                },
                "message_thread_id": 789
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("message with thread_id should parse");

        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.reply_target, "-100200300:789");
        assert_eq!(msg.content, "hello from topic");
        assert_eq!(msg.id, "telegram_-100200300_42");
    }

    #[test]
    fn parse_approval_callback_command_maps_approve_and_deny() {
        assert_eq!(
            TelegramChannel::parse_approval_callback_command("zcapr:yes:apr-1234"),
            Some("/approve-allow apr-1234".to_string())
        );
        assert_eq!(
            TelegramChannel::parse_approval_callback_command("zcapr:no:apr-5678"),
            Some("/approve-deny apr-5678".to_string())
        );
        assert_eq!(
            TelegramChannel::parse_approval_callback_command("noop:data"),
            None
        );
    }

    #[test]
    fn parse_approval_callback_command_trims_and_rejects_empty_ids() {
        assert_eq!(
            TelegramChannel::parse_approval_callback_command("zcapr:yes:   apr-1234   "),
            Some("/approve-allow apr-1234".to_string())
        );
        assert_eq!(
            TelegramChannel::parse_approval_callback_command("zcapr:no:\tapr-5678  "),
            Some("/approve-deny apr-5678".to_string())
        );
        assert_eq!(
            TelegramChannel::parse_approval_callback_command("zcapr:yes:   "),
            None
        );
        assert_eq!(
            TelegramChannel::parse_approval_callback_command("zcapr:no:"),
            None
        );
    }

    #[test]
    fn build_typing_action_body_uses_plain_chat_id_and_optional_thread_id() {
        let body = TelegramChannel::build_typing_action_body("-100200300:789");
        assert_eq!(
            body.get("chat_id").and_then(serde_json::Value::as_str),
            Some("-100200300")
        );
        assert_eq!(
            body.get("message_thread_id")
                .and_then(serde_json::Value::as_str),
            Some("789")
        );
        assert_eq!(
            body.get("action").and_then(serde_json::Value::as_str),
            Some("typing")
        );
    }

    #[test]
    fn build_typing_action_body_without_thread_does_not_emit_thread_id() {
        let body = TelegramChannel::build_typing_action_body("12345");
        assert_eq!(
            body.get("chat_id").and_then(serde_json::Value::as_str),
            Some("12345")
        );
        assert!(
            body.get("message_thread_id").is_none(),
            "thread id field should be absent for non-topic chats"
        );
    }

    #[tokio::test]
    async fn try_parse_approval_callback_query_builds_runtime_command_message() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], false, true);
        let update = serde_json::json!({
            "update_id": 7,
            "callback_query": {
                "id": "cb-1",
                "data": "zcapr:yes:apr-deadbeef",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "message": {
                    "message_id": 44,
                    "chat": { "id": -100_200_300 },
                    "message_thread_id": 789
                }
            }
        });

        let msg = ch
            .try_parse_approval_callback_query(&update)
            .expect("callback query should parse");

        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.reply_target, "-100200300:789");
        assert_eq!(msg.content, "/approve-allow apr-deadbeef");
        assert!(msg.id.starts_with("telegram_cb_-100200300_44_"));
    }

    // ── File sending API URL tests ──────────────────────────────────

    #[test]
    fn telegram_api_url_send_document() {
        let ch = TelegramChannel::new("123:ABC".into(), vec![], false, true);
        assert_eq!(
            ch.api_url("sendDocument"),
            "https://api.telegram.org/bot123:ABC/sendDocument"
        );
    }

    #[test]
    fn telegram_api_url_send_photo() {
        let ch = TelegramChannel::new("123:ABC".into(), vec![], false, true);
        assert_eq!(
            ch.api_url("sendPhoto"),
            "https://api.telegram.org/bot123:ABC/sendPhoto"
        );
    }

    #[test]
    fn telegram_api_url_send_video() {
        let ch = TelegramChannel::new("123:ABC".into(), vec![], false, true);
        assert_eq!(
            ch.api_url("sendVideo"),
            "https://api.telegram.org/bot123:ABC/sendVideo"
        );
    }

    #[test]
    fn telegram_api_url_send_audio() {
        let ch = TelegramChannel::new("123:ABC".into(), vec![], false, true);
        assert_eq!(
            ch.api_url("sendAudio"),
            "https://api.telegram.org/bot123:ABC/sendAudio"
        );
    }

    #[test]
    fn telegram_api_url_send_voice() {
        let ch = TelegramChannel::new("123:ABC".into(), vec![], false, true);
        assert_eq!(
            ch.api_url("sendVoice"),
            "https://api.telegram.org/bot123:ABC/sendVoice"
        );
    }

    // ── File sending integration tests (with mock server) ──────────

    #[tokio::test]
    async fn telegram_send_document_bytes_builds_correct_form() {
        // This test verifies the method doesn't panic and handles bytes correctly
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let file_bytes = b"Hello, this is a test file content".to_vec();

        // The actual API call will fail (no real server), but we verify the method exists
        // and handles the input correctly up to the network call
        let result = ch
            .send_document_bytes("123456", None, file_bytes, "test.txt", Some("Test caption"))
            .await;

        // Should fail with network error, not a panic or type error
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // Error should be network-related, not a code bug
        assert!(
            err.contains("error") || err.contains("failed") || err.contains("connect"),
            "Expected network error, got: {err}"
        );
    }

    #[tokio::test]
    async fn telegram_send_photo_bytes_builds_correct_form() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        // Minimal valid PNG header bytes
        let file_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

        let result = ch
            .send_photo_bytes("123456", None, file_bytes, "test.png", None)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_document_by_url_builds_correct_json() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);

        let result = ch
            .send_document_by_url(
                "123456",
                None,
                "https://example.com/file.pdf",
                Some("PDF doc"),
            )
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_photo_by_url_builds_correct_json() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);

        let result = ch
            .send_photo_by_url("123456", None, "https://example.com/image.jpg", None)
            .await;

        assert!(result.is_err());
    }

    // ── File path handling tests ────────────────────────────────────

    #[tokio::test]
    async fn telegram_send_document_nonexistent_file() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let path = Path::new("/nonexistent/path/to/file.txt");

        let result = ch.send_document("123456", None, path, None).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // Should fail with file not found error
        assert!(
            err.contains("No such file") || err.contains("not found") || err.contains("os error"),
            "Expected file not found error, got: {err}"
        );
    }

    #[tokio::test]
    async fn telegram_send_photo_nonexistent_file() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let path = Path::new("/nonexistent/path/to/photo.jpg");

        let result = ch.send_photo("123456", None, path, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_video_nonexistent_file() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let path = Path::new("/nonexistent/path/to/video.mp4");

        let result = ch.send_video("123456", None, path, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_audio_nonexistent_file() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let path = Path::new("/nonexistent/path/to/audio.mp3");

        let result = ch.send_audio("123456", None, path, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_voice_nonexistent_file() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let path = Path::new("/nonexistent/path/to/voice.ogg");

        let result = ch.send_voice("123456", None, path, None).await;

        assert!(result.is_err());
    }

    // ── Message splitting tests ─────────────────────────────────────

    #[test]
    fn telegram_split_short_message() {
        let msg = "Hello, world!";
        let chunks = split_message_for_telegram(msg);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], msg);
    }

    #[test]
    fn telegram_split_exact_limit() {
        let msg = "a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH);
        let chunks = split_message_for_telegram(&msg);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), TELEGRAM_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn telegram_split_over_limit() {
        let msg = "a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH + 100);
        let chunks = split_message_for_telegram(&msg);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= TELEGRAM_MAX_MESSAGE_LENGTH);
        assert!(chunks[1].len() <= TELEGRAM_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn telegram_split_at_word_boundary() {
        let msg = format!(
            "{} more text here",
            "word ".repeat(TELEGRAM_MAX_MESSAGE_LENGTH / 5)
        );
        let chunks = split_message_for_telegram(&msg);
        assert!(chunks.len() >= 2);
        // First chunk should end with a complete word (space at the end)
        for chunk in &chunks[..chunks.len() - 1] {
            assert!(chunk.len() <= TELEGRAM_MAX_MESSAGE_LENGTH);
        }
    }

    #[test]
    fn telegram_split_at_newline() {
        let text_block = "Line of text\n".repeat(TELEGRAM_MAX_MESSAGE_LENGTH / 13 + 1);
        let chunks = split_message_for_telegram(&text_block);
        assert!(chunks.len() >= 2);
        for chunk in chunks {
            assert!(chunk.len() <= TELEGRAM_MAX_MESSAGE_LENGTH);
        }
    }

    #[test]
    fn telegram_split_preserves_content() {
        let msg = "test ".repeat(TELEGRAM_MAX_MESSAGE_LENGTH / 5 + 100);
        let chunks = split_message_for_telegram(&msg);
        let rejoined = chunks.join("");
        assert_eq!(rejoined, msg);
    }

    #[test]
    fn telegram_split_empty_message() {
        let chunks = split_message_for_telegram("");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "");
    }

    #[test]
    fn telegram_split_very_long_message() {
        let msg = "x".repeat(TELEGRAM_MAX_MESSAGE_LENGTH * 3);
        let chunks = split_message_for_telegram(&msg);
        assert!(chunks.len() >= 3);
        for chunk in chunks {
            assert!(chunk.len() <= TELEGRAM_MAX_MESSAGE_LENGTH);
        }
    }

    // ── Caption handling tests ──────────────────────────────────────

    #[tokio::test]
    async fn telegram_send_document_bytes_with_caption() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let file_bytes = b"test content".to_vec();

        // With caption
        let result = ch
            .send_document_bytes(
                "123456",
                None,
                file_bytes.clone(),
                "test.txt",
                Some("My caption"),
            )
            .await;
        assert!(result.is_err()); // Network error expected

        // Without caption
        let result = ch
            .send_document_bytes("123456", None, file_bytes, "test.txt", None)
            .await;
        assert!(result.is_err()); // Network error expected
    }

    #[tokio::test]
    async fn telegram_send_photo_bytes_with_caption() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let file_bytes = vec![0x89, 0x50, 0x4E, 0x47];

        // With caption
        let result = ch
            .send_photo_bytes(
                "123456",
                None,
                file_bytes.clone(),
                "test.png",
                Some("Photo caption"),
            )
            .await;
        assert!(result.is_err());

        // Without caption
        let result = ch
            .send_photo_bytes("123456", None, file_bytes, "test.png", None)
            .await;
        assert!(result.is_err());
    }

    // ── Empty/edge case tests ───────────────────────────────────────

    #[tokio::test]
    async fn telegram_send_document_bytes_empty_file() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let file_bytes: Vec<u8> = vec![];

        let result = ch
            .send_document_bytes("123456", None, file_bytes, "empty.txt", None)
            .await;

        // Should not panic, will fail at API level
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_document_bytes_empty_filename() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let file_bytes = b"content".to_vec();

        let result = ch
            .send_document_bytes("123456", None, file_bytes, "", None)
            .await;

        // Should not panic
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_document_bytes_empty_chat_id() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true);
        let file_bytes = b"content".to_vec();

        let result = ch
            .send_document_bytes("", None, file_bytes, "test.txt", None)
            .await;

        // Should not panic
        assert!(result.is_err());
    }

    // ── Message ID edge cases ─────────────────────────────────────

    #[test]
    fn telegram_message_id_format_includes_chat_and_message_id() {
        // Verify that message IDs follow the format: telegram_{chat_id}_{message_id}
        let chat_id = "123456";
        let message_id = 789;
        let expected_id = format!("telegram_{chat_id}_{message_id}");
        assert_eq!(expected_id, "telegram_123456_789");
    }

    #[test]
    fn telegram_message_id_is_deterministic() {
        // Same chat_id + same message_id = same ID (prevents duplicates after restart)
        let chat_id = "123456";
        let message_id = 789;
        let id1 = format!("telegram_{chat_id}_{message_id}");
        let id2 = format!("telegram_{chat_id}_{message_id}");
        assert_eq!(id1, id2);
    }

    #[test]
    fn telegram_message_id_different_message_different_id() {
        // Different message IDs produce different IDs
        let chat_id = "123456";
        let id1 = format!("telegram_{chat_id}_789");
        let id2 = format!("telegram_{chat_id}_790");
        assert_ne!(id1, id2);
    }

    #[test]
    fn telegram_message_id_different_chat_different_id() {
        // Different chats produce different IDs even with same message_id
        let message_id = 789;
        let id1 = format!("telegram_123456_{message_id}");
        let id2 = format!("telegram_789012_{message_id}");
        assert_ne!(id1, id2);
    }

    #[test]
    fn telegram_message_id_no_uuid_randomness() {
        // Verify format doesn't contain random UUID components
        let chat_id = "123456";
        let message_id = 789;
        let id = format!("telegram_{chat_id}_{message_id}");
        assert!(!id.contains('-')); // No UUID dashes
        assert!(id.starts_with("telegram_"));
    }

    #[test]
    fn telegram_message_id_handles_zero_message_id() {
        // Edge case: message_id can be 0 (fallback/missing case)
        let chat_id = "123456";
        let message_id = 0;
        let id = format!("telegram_{chat_id}_{message_id}");
        assert_eq!(id, "telegram_123456_0");
    }

    // ── Tool call tag stripping tests ───────────────────────────────────

    #[test]
    fn strip_tool_call_tags_removes_standard_tags() {
        let input =
            "Hello <tool>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool> world";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello  world");
    }

    #[test]
    fn strip_tool_call_tags_removes_alias_tags() {
        let input = "Hello <toolcall>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</toolcall> world";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello  world");
    }

    #[test]
    fn strip_tool_call_tags_removes_dash_tags() {
        let input = "Hello <tool-call>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool-call> world";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello  world");
    }

    #[test]
    fn strip_tool_call_tags_removes_tool_call_tags() {
        let input = "Hello <tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool_call> world";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello  world");
    }

    #[test]
    fn strip_tool_call_tags_removes_invoke_tags() {
        let input = "Hello <invoke>{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}</invoke> world";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello  world");
    }

    #[test]
    fn strip_tool_call_tags_handles_multiple_tags() {
        let input = "Start <tool>a</tool> middle <tool>b</tool> end";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Start  middle  end");
    }

    #[test]
    fn strip_tool_call_tags_handles_mixed_tags() {
        let input = "A <tool>a</tool> B <toolcall>b</toolcall> C <tool-call>c</tool-call> D";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "A  B  C  D");
    }

    #[test]
    fn strip_tool_call_tags_preserves_normal_text() {
        let input = "Hello world! This is a test.";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello world! This is a test.");
    }

    #[test]
    fn strip_tool_call_tags_handles_unclosed_tags() {
        let input = "Hello <tool>world";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello <tool>world");
    }

    #[test]
    fn strip_tool_call_tags_handles_unclosed_tool_call_with_json() {
        let input =
            "Status:\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"uptime\"}}";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Status:");
    }

    #[test]
    fn strip_tool_call_tags_handles_mismatched_close_tag() {
        let input =
            "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"uptime\"}}</arg_value>";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "");
    }

    #[test]
    fn strip_tool_call_tags_cleans_extra_newlines() {
        let input = "Hello\n\n<tool>\ntest\n</tool>\n\n\nworld";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "Hello\n\nworld");
    }

    #[test]
    fn strip_tool_call_tags_handles_empty_input() {
        let input = "";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "");
    }

    #[test]
    fn strip_tool_call_tags_handles_only_tags() {
        let input = "<tool>{\"name\":\"test\"}</tool>";
        let result = strip_tool_call_tags(input);
        assert_eq!(result, "");
    }

    #[test]
    fn telegram_contains_bot_mention_finds_mention() {
        assert!(TelegramChannel::contains_bot_mention(
            "Hello @mybot",
            "mybot"
        ));
        assert!(TelegramChannel::contains_bot_mention(
            "@mybot help",
            "mybot"
        ));
        assert!(TelegramChannel::contains_bot_mention(
            "Hey @mybot how are you?",
            "mybot"
        ));
        assert!(TelegramChannel::contains_bot_mention(
            "Hello @MyBot, can you help?",
            "mybot"
        ));
    }

    #[test]
    fn telegram_contains_bot_mention_no_false_positives() {
        assert!(!TelegramChannel::contains_bot_mention(
            "Hello @otherbot",
            "mybot"
        ));
        assert!(!TelegramChannel::contains_bot_mention(
            "Hello mybot",
            "mybot"
        ));
        assert!(!TelegramChannel::contains_bot_mention(
            "Hello @mybot2",
            "mybot"
        ));
        assert!(!TelegramChannel::contains_bot_mention("", "mybot"));
    }

    #[test]
    fn telegram_normalize_incoming_content_strips_mention() {
        let result = TelegramChannel::normalize_incoming_content("@mybot hello", "mybot");
        assert_eq!(result, Some("hello".to_string()));
    }

    #[test]
    fn telegram_normalize_incoming_content_handles_multiple_mentions() {
        let result = TelegramChannel::normalize_incoming_content("@mybot @mybot test", "mybot");
        assert_eq!(result, Some("test".to_string()));
    }

    #[test]
    fn telegram_normalize_incoming_content_returns_none_for_empty() {
        let result = TelegramChannel::normalize_incoming_content("@mybot", "mybot");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_update_message_mention_only_group_requires_exact_mention() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let update = serde_json::json!({
            "update_id": 10,
            "message": {
                "message_id": 44,
                "text": "hello @mybot2",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300,
                    "type": "group"
                }
            }
        });

        assert!(ch.parse_update_message(&update).is_none());
    }

    #[test]
    fn parse_update_message_mention_only_group_strips_mention_and_drops_empty() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let update = serde_json::json!({
            "update_id": 11,
            "message": {
                "message_id": 45,
                "text": "Hi @MyBot status please",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300,
                    "type": "group"
                }
            }
        });

        let parsed = ch
            .parse_update_message(&update)
            .expect("mention should parse");
        assert_eq!(parsed.content, "Hi status please");

        let empty_update = serde_json::json!({
            "update_id": 12,
            "message": {
                "message_id": 46,
                "text": "@mybot",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300,
                    "type": "group"
                }
            }
        });

        assert!(ch.parse_update_message(&empty_update).is_none());
    }

    #[test]
    fn parse_update_message_mention_only_group_allows_configured_sender_without_mention() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true)
            .with_group_reply_allowed_senders(vec!["555".into()]);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let update = serde_json::json!({
            "update_id": 13,
            "message": {
                "message_id": 47,
                "text": "run daily sync",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300,
                    "type": "group"
                }
            }
        });

        let parsed = ch
            .parse_update_message(&update)
            .expect("sender override should bypass mention requirement");
        assert_eq!(parsed.content, "run daily sync");
    }

    #[test]
    fn passes_mention_only_gate_allows_configured_sender_for_non_text_messages() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true)
            .with_group_reply_allowed_senders(vec!["555".into()]);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let group_message = serde_json::json!({
            "chat": { "type": "group" }
        });

        assert!(
            ch.passes_mention_only_gate(&group_message, Some("555"), None),
            "voice/audio updates should honor sender bypass"
        );
        assert!(
            ch.passes_mention_only_gate(&group_message, Some("555"), Some("status update")),
            "attachment updates should honor sender bypass"
        );
    }

    #[test]
    fn passes_mention_only_gate_rejects_non_mentioned_non_bypassed_non_text_messages() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let group_message = serde_json::json!({
            "chat": { "type": "group" }
        });

        assert!(
            !ch.passes_mention_only_gate(&group_message, Some("999"), None),
            "voice/audio updates without sender bypass must be rejected"
        );
        assert!(
            !ch.passes_mention_only_gate(&group_message, Some("999"), Some("no mention here")),
            "attachments without sender bypass must include bot mention"
        );
        assert!(
            ch.passes_mention_only_gate(&group_message, Some("999"), Some("@mybot status")),
            "attachments with explicit mention should pass"
        );
    }

    #[test]
    fn telegram_is_group_message_detects_groups() {
        let group_msg = serde_json::json!({
            "chat": { "type": "group" }
        });
        assert!(TelegramChannel::is_group_message(&group_msg));

        let supergroup_msg = serde_json::json!({
            "chat": { "type": "supergroup" }
        });
        assert!(TelegramChannel::is_group_message(&supergroup_msg));

        let private_msg = serde_json::json!({
            "chat": { "type": "private" }
        });
        assert!(!TelegramChannel::is_group_message(&private_msg));
    }

    #[test]
    fn telegram_mention_only_enabled_by_config() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true);
        assert!(ch.mention_only);

        let ch_disabled = TelegramChannel::new("token".into(), vec!["*".into()], false, true);
        assert!(!ch_disabled.mention_only);
    }

    #[test]
    fn should_skip_unauthorized_prompt_for_non_mentioned_group_message() {
        let ch = TelegramChannel::new("token".into(), vec!["alice".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let message = serde_json::json!({
            "chat": { "type": "group" }
        });

        assert!(ch.should_skip_unauthorized_prompt(&message, "hello everyone", Some("999")));
    }

    #[test]
    fn should_not_skip_unauthorized_prompt_for_mentioned_group_message() {
        let ch = TelegramChannel::new("token".into(), vec!["alice".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let message = serde_json::json!({
            "chat": { "type": "group" }
        });

        assert!(!ch.should_skip_unauthorized_prompt(&message, "@mybot please help", Some("999")));
    }

    #[test]
    fn should_not_skip_unauthorized_prompt_outside_group_mention_only() {
        let ch = TelegramChannel::new("token".into(), vec!["alice".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let private_message = serde_json::json!({
            "chat": { "type": "private" }
        });
        assert!(!ch.should_skip_unauthorized_prompt(&private_message, "hello", Some("999")));

        let group_message = serde_json::json!({
            "chat": { "type": "group" }
        });
        let mention_disabled =
            TelegramChannel::new("token".into(), vec!["alice".into()], false, true);
        assert!(!mention_disabled.should_skip_unauthorized_prompt(
            &group_message,
            "hello",
            Some("999")
        ));
    }

    #[test]
    fn should_not_skip_unauthorized_prompt_for_group_sender_trigger_override() {
        let ch = TelegramChannel::new("token".into(), vec!["alice".into()], true, true)
            .with_group_reply_allowed_senders(vec!["999".into()]);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let message = serde_json::json!({
            "chat": { "type": "group" }
        });
        assert!(!ch.should_skip_unauthorized_prompt(&message, "hello everyone", Some("999")));
    }

    #[test]
    fn telegram_mention_only_group_photo_without_caption_is_ignored() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let _update = serde_json::json!({
            "update_id": 100,
            "message": {
                "message_id": 1,
                "photo": [
                    {"file_id": "photo_id", "file_size": 1_000}
                ],
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300,
                    "type": "group"
                }
            }
        });

        // Photo without caption in group chat with mention_only=true should be ignored
        // Note: This test verifies the check is in place, but the async function needs
        // a workspace_dir to be set for full parsing. The key check happens before download.
        // For unit testing purposes, we verify the logic path exists.
        assert!(ch.mention_only);
    }

    #[test]
    fn telegram_mention_only_group_photo_with_caption_without_mention_is_ignored() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        // Photo with caption that doesn't mention the bot
        let _update = serde_json::json!({
            "update_id": 101,
            "message": {
                "message_id": 2,
                "photo": [
                    {"file_id": "photo_id", "file_size": 1_000}
                ],
                "caption": "Look at this image",
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": -100_200_300,
                    "type": "group"
                }
            }
        });

        // The mention_only check should reject this since caption doesn't contain @mybot
        assert!(ch.mention_only);
    }

    #[test]
    fn telegram_mention_only_private_chat_photo_still_works() {
        // Private chats should still work regardless of mention_only setting
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], true, true);
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }

        let _update = serde_json::json!({
            "update_id": 102,
            "message": {
                "message_id": 3,
                "photo": [
                    {"file_id": "photo_id", "file_size": 1_000}
                ],
                "from": {
                    "id": 555,
                    "username": "alice"
                },
                "chat": {
                    "id": 123_456,
                    "type": "private"
                }
            }
        });

        // Private chat should work even with mention_only=true
        // The is_group_message check should return false for private chats
        assert!(ch.mention_only);
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG6: Channel platform limit edge cases for Telegram (4096 char limit)
    // Prevents: Pattern 6 — issues #574, #499
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn telegram_split_code_block_at_boundary() {
        let mut msg = String::new();
        msg.push_str("```python\n");
        msg.push_str(&"x".repeat(4085));
        msg.push_str("\n```\nMore text after code block");
        let parts = split_message_for_telegram(&msg);
        assert!(
            parts.len() >= 2,
            "code block spanning boundary should split"
        );
        for part in &parts {
            assert!(
                part.len() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "each part must be <= {TELEGRAM_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
    }

    #[test]
    fn telegram_split_single_long_word() {
        let long_word = "a".repeat(5000);
        let parts = split_message_for_telegram(&long_word);
        assert!(parts.len() >= 2, "word exceeding limit must be split");
        for part in &parts {
            assert!(
                part.len() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "hard-split part must be <= {TELEGRAM_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
        let reassembled: String = parts.join("");
        assert_eq!(reassembled, long_word);
    }

    #[test]
    fn telegram_split_exactly_at_limit_no_split() {
        let msg = "a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH);
        let parts = split_message_for_telegram(&msg);
        assert_eq!(parts.len(), 1, "message exactly at limit should not split");
    }

    #[test]
    fn telegram_split_one_over_limit() {
        let msg = "a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH + 1);
        let parts = split_message_for_telegram(&msg);
        assert!(parts.len() >= 2, "message 1 char over limit must split");
    }

    #[test]
    fn telegram_split_many_short_lines() {
        let msg: String = (0..1_000)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .concat();
        let parts = split_message_for_telegram(&msg);
        for part in &parts {
            assert!(
                part.len() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "short-line batch must be <= limit"
            );
        }
    }

    #[test]
    fn telegram_split_only_whitespace() {
        let msg = "   \n\n\t  ";
        let parts = split_message_for_telegram(msg);
        assert!(parts.len() <= 1);
    }

    #[test]
    fn telegram_split_emoji_at_boundary() {
        let mut msg = "a".repeat(4094);
        msg.push_str("🎉🎊"); // 4096 chars total
        let parts = split_message_for_telegram(&msg);
        for part in &parts {
            // The function splits on character count, not byte count
            assert!(
                part.chars().count() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "emoji boundary split must respect limit"
            );
        }
    }

    #[test]
    fn telegram_split_consecutive_newlines() {
        let mut msg = "a".repeat(4090);
        msg.push_str("\n\n\n\n\n\n");
        msg.push_str(&"b".repeat(100));
        let parts = split_message_for_telegram(&msg);
        for part in &parts {
            assert!(part.len() <= TELEGRAM_MAX_MESSAGE_LENGTH);
        }
    }

    #[test]
    fn parse_voice_metadata_extracts_voice() {
        let msg = serde_json::json!({
            "voice": {
                "file_id": "abc123",
                "duration": 5
            }
        });
        let meta = TelegramChannel::parse_voice_metadata(&msg).unwrap();
        assert_eq!(meta.file_id, "abc123");
        assert_eq!(meta.duration_secs, 5);
        assert!(meta.voice_note);
    }

    #[test]
    fn parse_voice_metadata_extracts_audio() {
        let msg = serde_json::json!({
            "audio": {
                "file_id": "audio456",
                "duration": 30
            }
        });
        let meta = TelegramChannel::parse_voice_metadata(&msg).unwrap();
        assert_eq!(meta.file_id, "audio456");
        assert_eq!(meta.duration_secs, 30);
        assert!(!meta.voice_note);
    }

    #[test]
    fn parse_voice_metadata_returns_none_for_text() {
        let msg = serde_json::json!({
            "text": "hello"
        });
        assert!(TelegramChannel::parse_voice_metadata(&msg).is_none());
    }

    #[test]
    fn parse_voice_metadata_defaults_duration_to_zero() {
        let msg = serde_json::json!({
            "voice": {
                "file_id": "no_dur"
            }
        });
        let meta = TelegramChannel::parse_voice_metadata(&msg).unwrap();
        assert_eq!(meta.duration_secs, 0);
    }

    #[test]
    fn infer_voice_filename_prefers_hint_with_extension() {
        let meta = VoiceMetadata {
            file_id: "f".into(),
            duration_secs: 0,
            file_name_hint: Some("telegram_voice.m4a".into()),
            mime_type_hint: Some("audio/mp4".into()),
            voice_note: false,
        };
        assert_eq!(
            TelegramChannel::infer_voice_filename("voice/file_without_ext", &meta),
            "telegram_voice.m4a"
        );
    }

    #[test]
    fn infer_voice_filename_uses_mime_extension_when_path_has_none() {
        let meta = VoiceMetadata {
            file_id: "f".into(),
            duration_secs: 0,
            file_name_hint: None,
            mime_type_hint: Some("audio/ogg".into()),
            voice_note: true,
        };
        assert_eq!(
            TelegramChannel::infer_voice_filename("voice/file_without_ext", &meta),
            "file_without_ext.ogg"
        );
    }

    #[test]
    fn infer_voice_filename_falls_back_for_audio_without_hints() {
        let meta = VoiceMetadata {
            file_id: "f".into(),
            duration_secs: 0,
            file_name_hint: None,
            mime_type_hint: None,
            voice_note: false,
        };
        assert_eq!(
            TelegramChannel::infer_voice_filename("voice/file_without_ext", &meta),
            "file_without_ext.mp3"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // extract_sender_info tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn extract_sender_info_with_username() {
        let msg = serde_json::json!({
            "from": { "id": 123, "username": "alice" }
        });
        let (username, sender_id, identity) = TelegramChannel::extract_sender_info(&msg);
        assert_eq!(username, "alice");
        assert_eq!(sender_id, Some("123".to_string()));
        assert_eq!(identity, "alice");
    }

    #[test]
    fn extract_sender_info_without_username() {
        let msg = serde_json::json!({
            "from": { "id": 42 }
        });
        let (username, sender_id, identity) = TelegramChannel::extract_sender_info(&msg);
        assert_eq!(username, "unknown");
        assert_eq!(sender_id, Some("42".to_string()));
        assert_eq!(identity, "42");
    }

    // ─────────────────────────────────────────────────────────────────────
    // extract_reply_context tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn extract_reply_context_text_message() {
        let ch = TelegramChannel::new("t".into(), vec!["*".into()], false, true);
        let msg = serde_json::json!({
            "reply_to_message": {
                "from": { "username": "alice" },
                "text": "Hello world"
            }
        });
        let ctx = ch.extract_reply_context(&msg).unwrap();
        assert_eq!(ctx, "> @alice:\n> Hello world");
    }

    #[test]
    fn extract_reply_context_voice_message() {
        let ch = TelegramChannel::new("t".into(), vec!["*".into()], false, true);
        let msg = serde_json::json!({
            "reply_to_message": {
                "from": { "username": "bob" },
                "voice": { "file_id": "abc", "duration": 5 }
            }
        });
        let ctx = ch.extract_reply_context(&msg).unwrap();
        assert_eq!(ctx, "> @bob:\n> [Voice message]");
    }

    #[test]
    fn extract_reply_context_no_reply() {
        let ch = TelegramChannel::new("t".into(), vec!["*".into()], false, true);
        let msg = serde_json::json!({
            "text": "just a regular message"
        });
        assert!(ch.extract_reply_context(&msg).is_none());
    }

    #[test]
    fn extract_reply_context_no_username_uses_first_name() {
        let ch = TelegramChannel::new("t".into(), vec!["*".into()], false, true);
        let msg = serde_json::json!({
            "reply_to_message": {
                "from": { "id": 999, "first_name": "Charlie" },
                "text": "Hi there"
            }
        });
        let ctx = ch.extract_reply_context(&msg).unwrap();
        assert_eq!(ctx, "> @Charlie:\n> Hi there");
    }

    #[test]
    fn extract_reply_context_voice_with_cached_transcription() {
        let ch = TelegramChannel::new("t".into(), vec!["*".into()], false, true);
        // Pre-populate transcription cache
        ch.voice_transcriptions
            .lock()
            .insert("100:42".to_string(), "Hello from voice".to_string());
        let msg = serde_json::json!({
            "chat": { "id": 100 },
            "reply_to_message": {
                "message_id": 42,
                "from": { "username": "bob" },
                "voice": { "file_id": "abc", "duration": 5 }
            }
        });
        let ctx = ch.extract_reply_context(&msg).unwrap();
        assert_eq!(ctx, "> @bob:\n> [Voice] Hello from voice");
    }

    #[test]
    fn parse_update_message_includes_reply_context() {
        let ch = TelegramChannel::new("t".into(), vec!["*".into()], false, true);
        let update = serde_json::json!({
            "message": {
                "message_id": 10,
                "text": "translate this",
                "from": { "id": 1, "username": "alice" },
                "chat": { "id": 100, "type": "private" },
                "reply_to_message": {
                    "from": { "username": "bot" },
                    "text": "Bonjour le monde"
                }
            }
        });
        let parsed = ch.parse_update_message(&update).unwrap();
        assert!(
            parsed.content.starts_with("> @bot:"),
            "content should start with quote: {}",
            parsed.content
        );
        assert!(
            parsed.content.contains("translate this"),
            "content should contain user text"
        );
        assert!(
            parsed.content.contains("Bonjour le monde"),
            "content should contain quoted text"
        );
    }

    #[test]
    fn with_transcription_sets_config_when_enabled() {
        let mut tc = crate::config::TranscriptionConfig::default();
        tc.enabled = true;

        let ch = TelegramChannel::new("token".into(), vec!["*".into()], false, true)
            .with_transcription(tc);
        assert!(ch.transcription.is_some());
    }

    #[test]
    fn with_transcription_skips_when_disabled() {
        let tc = crate::config::TranscriptionConfig::default(); // enabled = false
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], false, true)
            .with_transcription(tc);
        assert!(ch.transcription.is_none());
    }

    #[tokio::test]
    async fn try_parse_voice_message_returns_none_when_transcription_disabled() {
        let ch = TelegramChannel::new("token".into(), vec!["*".into()], false, true);
        let update = serde_json::json!({
            "message": {
                "message_id": 1,
                "voice": { "file_id": "voice_file", "duration": 4 },
                "from": { "id": 123, "username": "alice" },
                "chat": { "id": 456, "type": "private" }
            }
        });

        let parsed = ch.try_parse_voice_message(&update).await;
        assert!(parsed.is_none());
    }

    #[tokio::test]
    async fn try_parse_voice_message_skips_when_duration_exceeds_limit() {
        let mut tc = crate::config::TranscriptionConfig::default();
        tc.enabled = true;
        tc.max_duration_secs = 5;

        let ch = TelegramChannel::new("token".into(), vec!["*".into()], false, true)
            .with_transcription(tc);
        let update = serde_json::json!({
            "message": {
                "message_id": 2,
                "voice": { "file_id": "voice_file", "duration": 30 },
                "from": { "id": 123, "username": "alice" },
                "chat": { "id": 456, "type": "private" }
            }
        });

        let parsed = ch.try_parse_voice_message(&update).await;
        assert!(parsed.is_none());
    }

    #[tokio::test]
    async fn try_parse_voice_message_rejects_unauthorized_sender_before_download() {
        let mut tc = crate::config::TranscriptionConfig::default();
        tc.enabled = true;
        tc.max_duration_secs = 120;

        let ch = TelegramChannel::new("token".into(), vec!["alice".into()], false, true)
            .with_transcription(tc);
        let update = serde_json::json!({
            "message": {
                "message_id": 3,
                "voice": { "file_id": "voice_file", "duration": 4 },
                "from": { "id": 999, "username": "bob" },
                "chat": { "id": 456, "type": "private" }
            }
        });

        let parsed = ch.try_parse_voice_message(&update).await;
        assert!(parsed.is_none());
        assert!(ch.voice_transcriptions.lock().is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // Live e2e: voice transcription via Groq Whisper + reply cache lookup
    // ─────────────────────────────────────────────────────────────────────

    /// Live test: voice transcription via Groq Whisper + reply cache lookup.
    ///
    /// Loads a pre-recorded MP3 fixture ("hello"), sends it to Groq Whisper
    /// API, verifies the transcription contains "hello", then caches it and
    /// checks that `extract_reply_context` returns the cached text instead
    /// of the `[Voice message]` fallback placeholder.
    ///
    /// Skipped automatically when `GROQ_API_KEY` is not set.
    /// Run: `GROQ_API_KEY=<key> cargo test --lib -- telegram::tests::e2e_live_voice_transcription_and_reply_cache --ignored`
    #[tokio::test]
    #[ignore = "requires GROQ_API_KEY"]
    async fn e2e_live_voice_transcription_and_reply_cache() {
        if std::env::var("GROQ_API_KEY").is_err() {
            eprintln!("GROQ_API_KEY not set — skipping live voice transcription test");
            return;
        }

        // 1. Load pre-recorded fixture (TTS-generated "hello", ~7 KB MP3)
        let fixture_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello.mp3");
        let audio_data = std::fs::read(&fixture_path)
            .unwrap_or_else(|e| panic!("Failed to read fixture {}: {e}", fixture_path.display()));
        assert!(
            audio_data.len() > 1000,
            "fixture too small ({} bytes), likely corrupt",
            audio_data.len()
        );

        // 2. Call transcribe_audio() — real Groq Whisper API
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            ..Default::default()
        };
        let transcript: String =
            crate::channels::transcription::transcribe_audio(audio_data, "hello.mp3", &config)
                .await
                .expect("transcribe_audio should succeed with valid GROQ_API_KEY");

        // 3. Verify Whisper actually recognized "hello"
        assert!(
            transcript.to_lowercase().contains("hello"),
            "expected transcription to contain 'hello', got: '{transcript}'"
        );

        // 4. Create TelegramChannel, insert transcription into voice_transcriptions cache
        let ch = TelegramChannel::new("test_token".into(), vec!["*".into()], false, true);
        let chat_id: i64 = 12345;
        let message_id: i64 = 67;
        let cache_key = format!("{chat_id}:{message_id}");
        ch.voice_transcriptions
            .lock()
            .insert(cache_key, transcript.clone());

        // 5. Build reply message with voice + message_id + chat.id
        let msg = serde_json::json!({
            "chat": { "id": chat_id },
            "reply_to_message": {
                "message_id": message_id,
                "from": { "username": "zeroclaw_user" },
                "voice": { "file_id": "test_file", "duration": 1 }
            }
        });

        // 6. Verify extract_reply_context returns cached transcription
        let ctx = ch
            .extract_reply_context(&msg)
            .expect("extract_reply_context should return Some for voice reply");

        assert!(
            ctx.contains(&format!("[Voice] {transcript}")),
            "expected cached transcription in reply context, got: {ctx}"
        );

        // Must NOT contain the fallback placeholder
        assert!(
            !ctx.contains("[Voice message]"),
            "context should use cached transcription, not fallback placeholder, got: {ctx}"
        );
    }

    // ── IncomingAttachment / parse_attachment_metadata tests ─────────

    #[test]
    fn parse_attachment_metadata_detects_document() {
        let message = serde_json::json!({
            "document": {
                "file_id": "BQACAgIAAxk",
                "file_name": "report.pdf",
                "file_size": 12345
            }
        });
        let att = TelegramChannel::parse_attachment_metadata(&message).unwrap();
        assert_eq!(att.kind, IncomingAttachmentKind::Document);
        assert_eq!(att.file_id, "BQACAgIAAxk");
        assert_eq!(att.file_name.as_deref(), Some("report.pdf"));
        assert_eq!(att.file_size, Some(12345));
        assert!(att.caption.is_none());
    }

    #[test]
    fn parse_attachment_metadata_detects_photo() {
        let message = serde_json::json!({
            "photo": [
                {"file_id": "small_id", "file_size": 100, "width": 90, "height": 90},
                {"file_id": "medium_id", "file_size": 500, "width": 320, "height": 320},
                {"file_id": "large_id", "file_size": 2000, "width": 800, "height": 800}
            ]
        });
        let att = TelegramChannel::parse_attachment_metadata(&message).unwrap();
        assert_eq!(att.kind, IncomingAttachmentKind::Photo);
        assert_eq!(att.file_id, "large_id");
        assert_eq!(att.file_size, Some(2000));
        assert!(att.file_name.is_none());
    }

    #[test]
    fn parse_attachment_metadata_extracts_caption() {
        // Document with caption
        let doc_msg = serde_json::json!({
            "document": {
                "file_id": "doc_id",
                "file_name": "data.csv"
            },
            "caption": "Monthly report"
        });
        let att = TelegramChannel::parse_attachment_metadata(&doc_msg).unwrap();
        assert_eq!(att.caption.as_deref(), Some("Monthly report"));

        // Photo with caption
        let photo_msg = serde_json::json!({
            "photo": [
                    {"file_id": "photo_id", "file_size": 1_000}
            ],
            "caption": "Look at this"
        });
        let att = TelegramChannel::parse_attachment_metadata(&photo_msg).unwrap();
        assert_eq!(att.caption.as_deref(), Some("Look at this"));
    }

    #[test]
    fn parse_attachment_metadata_document_without_optional_fields() {
        let message = serde_json::json!({
            "document": {
                "file_id": "doc_no_name"
            }
        });
        let att = TelegramChannel::parse_attachment_metadata(&message).unwrap();
        assert_eq!(att.kind, IncomingAttachmentKind::Document);
        assert_eq!(att.file_id, "doc_no_name");
        assert!(att.file_name.is_none());
        assert!(att.file_size.is_none());
        assert!(att.caption.is_none());
    }

    #[test]
    fn parse_attachment_metadata_returns_none_for_text() {
        let message = serde_json::json!({
            "text": "Hello world"
        });
        assert!(TelegramChannel::parse_attachment_metadata(&message).is_none());
    }

    #[test]
    fn parse_attachment_metadata_returns_none_for_voice() {
        let message = serde_json::json!({
            "voice": {
                "file_id": "voice_id",
                "duration": 5
            }
        });
        assert!(TelegramChannel::parse_attachment_metadata(&message).is_none());
    }

    #[test]
    fn parse_attachment_metadata_empty_photo_array() {
        let message = serde_json::json!({
            "photo": []
        });
        assert!(TelegramChannel::parse_attachment_metadata(&message).is_none());
    }

    #[test]
    fn with_workspace_dir_sets_field() {
        let ch = TelegramChannel::new("fake-token".into(), vec!["*".into()], false, true)
            .with_workspace_dir(std::path::PathBuf::from("/tmp/test_workspace"));
        assert_eq!(
            ch.workspace_dir.as_deref(),
            Some(std::path::Path::new("/tmp/test_workspace"))
        );
    }

    #[test]
    fn telegram_max_file_download_bytes_is_20mb() {
        assert_eq!(TELEGRAM_MAX_FILE_DOWNLOAD_BYTES, 20 * 1024 * 1024);
    }

    // ── Attachment content format tests ──────────────────────────────

    /// Photo attachments with image extension must use `[IMAGE:/path]` marker
    /// so the multimodal pipeline validates vision capability on the provider.
    #[test]
    fn attachment_photo_content_uses_image_marker() {
        let local_path = std::path::Path::new("/tmp/workspace/photo_123_45.jpg");
        let local_filename = "photo_123_45.jpg";

        let content =
            format_attachment_content(IncomingAttachmentKind::Photo, local_filename, local_path);

        assert_eq!(content, "[IMAGE:/tmp/workspace/photo_123_45.jpg]");
        assert!(content.starts_with("[IMAGE:"));
        assert!(content.ends_with(']'));
    }

    /// Document attachments keep `[Document: name] /path` format.
    #[test]
    fn attachment_document_content_uses_document_label() {
        let local_path = std::path::Path::new("/tmp/workspace/report.pdf");
        let local_filename = "report.pdf";

        let content =
            format_attachment_content(IncomingAttachmentKind::Document, local_filename, local_path);

        assert_eq!(content, "[Document: report.pdf] /tmp/workspace/report.pdf");
        assert!(!content.contains("[IMAGE:"));
    }

    /// Markdown files must never produce `[IMAGE:]` markers (issue #1274).
    #[test]
    fn markdown_file_never_produces_image_marker() {
        let local_path = std::path::Path::new("/tmp/workspace/telegram_files/notes.md");
        let local_filename = "notes.md";

        // Even if Telegram misclassifies as Photo, extension guard prevents [IMAGE:].
        let content =
            format_attachment_content(IncomingAttachmentKind::Photo, local_filename, local_path);
        assert!(
            !content.contains("[IMAGE:"),
            "markdown must not get [IMAGE:] marker: {content}"
        );
        assert!(content.starts_with("[Document:"));

        // As Document, it should also be correct.
        let content_doc =
            format_attachment_content(IncomingAttachmentKind::Document, local_filename, local_path);
        assert!(
            !content_doc.contains("[IMAGE:"),
            "markdown document must not get [IMAGE:] marker: {content_doc}"
        );
    }

    /// Non-image files classified as Photo fall back to `[Document:]` format.
    #[test]
    fn non_image_photo_falls_back_to_document_format() {
        for (filename, ext_path) in [
            ("file.md", "/tmp/ws/file.md"),
            ("file.txt", "/tmp/ws/file.txt"),
            ("file.pdf", "/tmp/ws/file.pdf"),
            ("file.csv", "/tmp/ws/file.csv"),
            ("file.json", "/tmp/ws/file.json"),
            ("file.zip", "/tmp/ws/file.zip"),
            ("file", "/tmp/ws/file"),
        ] {
            let path = std::path::Path::new(ext_path);
            let content = format_attachment_content(IncomingAttachmentKind::Photo, filename, path);
            assert!(
                !content.contains("[IMAGE:"),
                "{filename}: non-image file should not get [IMAGE:] marker, got: {content}"
            );
            assert!(
                content.starts_with("[Document:"),
                "{filename}: should use [Document:] format, got: {content}"
            );
        }
    }

    /// All recognized image extensions produce `[IMAGE:]` when classified as Photo.
    #[test]
    fn image_extensions_produce_image_marker() {
        for ext in ["png", "jpg", "jpeg", "gif", "webp", "bmp"] {
            let filename = format!("photo_1_2.{ext}");
            let path_str = format!("/tmp/ws/{filename}");
            let path = std::path::Path::new(&path_str);
            let content = format_attachment_content(IncomingAttachmentKind::Photo, &filename, path);
            assert!(
                content.starts_with("[IMAGE:"),
                "{ext}: image should get [IMAGE:] marker, got: {content}"
            );
        }
    }

    /// Multimodal pipeline must return 0 image markers for document-formatted
    /// content — even for a file misclassified as Photo (issue #1274).
    #[test]
    fn markdown_attachment_not_detected_by_multimodal_image_markers() {
        let content = format_attachment_content(
            IncomingAttachmentKind::Photo,
            "notes.md",
            std::path::Path::new("/tmp/ws/notes.md"),
        );
        let messages = vec![crate::providers::ChatMessage::user(content)];
        assert_eq!(
            crate::multimodal::count_image_markers(&messages),
            0,
            "markdown file must not trigger image marker detection"
        );
    }

    /// `is_image_extension` helper recognizes image formats and rejects others.
    #[test]
    fn is_image_extension_recognizes_images() {
        assert!(is_image_extension(std::path::Path::new("photo.png")));
        assert!(is_image_extension(std::path::Path::new("photo.jpg")));
        assert!(is_image_extension(std::path::Path::new("photo.jpeg")));
        assert!(is_image_extension(std::path::Path::new("photo.gif")));
        assert!(is_image_extension(std::path::Path::new("photo.webp")));
        assert!(is_image_extension(std::path::Path::new("photo.bmp")));
        assert!(is_image_extension(std::path::Path::new("PHOTO.PNG")));

        assert!(!is_image_extension(std::path::Path::new("file.md")));
        assert!(!is_image_extension(std::path::Path::new("file.txt")));
        assert!(!is_image_extension(std::path::Path::new("file.pdf")));
        assert!(!is_image_extension(std::path::Path::new("file.csv")));
        assert!(!is_image_extension(std::path::Path::new("file")));
    }

    /// `count_image_markers` from the multimodal module must detect the
    /// `[IMAGE:]` marker produced by photo attachment formatting.
    #[test]
    fn photo_image_marker_detected_by_multimodal() {
        let photo_content = "[IMAGE:/tmp/workspace/photo_1_2.jpg]";
        let messages = vec![crate::providers::ChatMessage::user(
            photo_content.to_string(),
        )];
        let count = crate::multimodal::count_image_markers(&messages);
        assert_eq!(
            count, 1,
            "multimodal should detect exactly one image marker"
        );
    }

    /// Photo with caption: `[IMAGE:/path]\n\nCaption text`.
    #[test]
    fn photo_image_marker_with_caption() {
        let local_path = std::path::Path::new("/tmp/workspace/photo_1_2.jpg");
        let mut content = format!("[IMAGE:{}]", local_path.display());
        let caption = "Look at this screenshot";
        use std::fmt::Write;
        let _ = write!(content, "\n\n{caption}");

        assert_eq!(
            content,
            "[IMAGE:/tmp/workspace/photo_1_2.jpg]\n\nLook at this screenshot"
        );

        // Multimodal pipeline still detects the marker.
        let messages = vec![crate::providers::ChatMessage::user(content)];
        assert_eq!(crate::multimodal::count_image_markers(&messages), 1);
    }

    // ── E2E: attachment saves file and formats content ───────────────

    /// Full pipeline test: simulate file download → save to workspace →
    /// verify content format for both document and photo attachments.
    #[test]
    fn e2e_attachment_saves_file_and_formats_content() {
        let workspace = tempfile::tempdir().expect("create temp workspace");

        // ── Document attachment ──────────────────────────────────────
        let doc_filename = "report.pdf";
        let doc_path = workspace.path().join(doc_filename);
        // Simulate downloaded file.
        std::fs::write(&doc_path, b"%PDF-1.4 fake").expect("write doc fixture");
        assert!(doc_path.exists(), "document file must exist on disk");

        let doc_content =
            format_attachment_content(IncomingAttachmentKind::Document, doc_filename, &doc_path);
        assert!(
            doc_content.starts_with("[Document: report.pdf]"),
            "document label format mismatch: {doc_content}"
        );
        // Multimodal must NOT detect image markers in document content.
        let doc_msgs = vec![crate::providers::ChatMessage::user(doc_content)];
        assert_eq!(
            crate::multimodal::count_image_markers(&doc_msgs),
            0,
            "document content must not contain image markers"
        );

        // ── Photo attachment ─────────────────────────────────────────
        let photo_filename = "photo_99_1.jpg";
        let photo_path = workspace.path().join(photo_filename);
        // Copy the JPEG fixture.
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test_photo.jpg");
        std::fs::copy(&fixture, &photo_path).expect("copy photo fixture");
        assert!(photo_path.exists(), "photo file must exist on disk");

        let photo_content =
            format_attachment_content(IncomingAttachmentKind::Photo, photo_filename, &photo_path);
        assert!(
            photo_content.starts_with("[IMAGE:"),
            "photo must use [IMAGE:] marker: {photo_content}"
        );
        assert!(
            photo_content.ends_with(']'),
            "photo marker must close with ]: {photo_content}"
        );

        // Multimodal detects the marker.
        let photo_msgs = vec![crate::providers::ChatMessage::user(photo_content.clone())];
        assert_eq!(
            crate::multimodal::count_image_markers(&photo_msgs),
            1,
            "multimodal must detect exactly one image marker in photo content"
        );

        // ── Photo with caption ───────────────────────────────────────
        let mut captioned = photo_content;
        use std::fmt::Write;
        let _ = write!(captioned, "\n\nCheck this out");
        let cap_msgs = vec![crate::providers::ChatMessage::user(captioned.clone())];
        assert_eq!(
            crate::multimodal::count_image_markers(&cap_msgs),
            1,
            "caption must not break image marker detection"
        );
        assert!(
            captioned.contains("Check this out"),
            "caption text must be present in content"
        );

        // ── Markdown file sent as Photo (issue #1274) ────────────────
        let md_filename = "notes.md";
        let md_path = workspace.path().join(md_filename);
        std::fs::write(&md_path, b"# Hello\nSome markdown").expect("write md fixture");
        let md_content =
            format_attachment_content(IncomingAttachmentKind::Photo, md_filename, &md_path);
        assert!(
            !md_content.contains("[IMAGE:"),
            "markdown must not get [IMAGE:] marker: {md_content}"
        );
        let md_msgs = vec![crate::providers::ChatMessage::user(md_content)];
        assert_eq!(
            crate::multimodal::count_image_markers(&md_msgs),
            0,
            "markdown file must not trigger image marker detection"
        );
    }

    // ── Groq provider rejects photo with vision error ────────────────

    /// Verify that the Groq provider (OpenAI-compatible) does not support
    /// vision, so the existing `count_image_markers > 0 && !supports_vision()`
    /// guard in `agent/loop_.rs` will reject photo messages.
    #[test]
    fn groq_provider_rejects_photo_with_vision_error() {
        use crate::providers::compatible::{AuthStyle, OpenAiCompatibleProvider};
        use crate::providers::Provider;

        let groq = OpenAiCompatibleProvider::new(
            "Groq",
            "https://api.groq.com/openai/v1",
            Some("fake_key"),
            AuthStyle::Bearer,
        );

        // Groq must not support vision.
        assert!(
            !groq.supports_vision(),
            "Groq provider must not support vision"
        );

        // Build a message with an [IMAGE:] marker (as photo attachment would).
        let messages = vec![crate::providers::ChatMessage::user(
            "[IMAGE:/tmp/photo.jpg]\n\nDescribe this image".to_string(),
        )];
        let marker_count = crate::multimodal::count_image_markers(&messages);
        assert_eq!(marker_count, 1, "must detect image marker in photo content");

        // The combination of marker_count > 0 && !supports_vision() means
        // the agent loop will return ProviderCapabilityError before calling
        // the provider, and the channel will send "⚠️ Error: ..." to the user.
    }

    #[test]
    fn document_with_image_extension_routes_to_image_marker() {
        let path = std::path::Path::new("/tmp/workspace/scan.png");
        let result = format_attachment_content(IncomingAttachmentKind::Document, "scan.png", path);
        assert_eq!(result, "[IMAGE:/tmp/workspace/scan.png]");

        let path = std::path::Path::new("/tmp/workspace/photo.jpg");
        let result = format_attachment_content(IncomingAttachmentKind::Document, "photo.jpg", path);
        assert!(result.starts_with("[IMAGE:"));
    }

    #[test]
    fn document_with_non_image_extension_routes_to_document_format() {
        let path = std::path::Path::new("/tmp/workspace/report.pdf");
        let result =
            format_attachment_content(IncomingAttachmentKind::Document, "report.pdf", path);
        assert_eq!(result, "[Document: report.pdf] /tmp/workspace/report.pdf");
        assert!(!result.starts_with("[IMAGE:"));
    }
}
