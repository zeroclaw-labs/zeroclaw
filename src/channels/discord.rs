use super::ack_reaction::{select_ack_reaction, AckReactionContext, AckReactionContextChatType};
use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::AckReactionConfig;
use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use reqwest::multipart::{Form, Part};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

/// Discord approval button custom_id prefixes.
const DISCORD_APPROVAL_APPROVE_PREFIX: &str = "zcapr:yes:";
const DISCORD_APPROVAL_DENY_PREFIX: &str = "zcapr:no:";

/// Discord channel — connects via Gateway WebSocket for real-time messages
pub struct DiscordChannel {
    bot_token: String,
    guild_id: Option<String>,
    allowed_users: Vec<String>,
    listen_to_bots: bool,
    mention_only: bool,
    group_reply_allowed_sender_ids: Vec<String>,
    ack_reaction: Option<AckReactionConfig>,
    workspace_dir: Option<PathBuf>,
    typing_handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
}

impl DiscordChannel {
    pub fn new(
        bot_token: String,
        guild_id: Option<String>,
        allowed_users: Vec<String>,
        listen_to_bots: bool,
        mention_only: bool,
    ) -> Self {
        Self {
            bot_token,
            guild_id,
            allowed_users,
            listen_to_bots,
            mention_only,
            group_reply_allowed_sender_ids: Vec::new(),
            ack_reaction: None,
            workspace_dir: None,
            typing_handles: Mutex::new(HashMap::new()),
        }
    }

    /// Configure sender IDs that bypass mention gating in guild channels.
    pub fn with_group_reply_allowed_senders(mut self, sender_ids: Vec<String>) -> Self {
        self.group_reply_allowed_sender_ids = normalize_group_reply_allowed_sender_ids(sender_ids);
        self
    }

    /// Configure ACK reaction policy.
    pub fn with_ack_reaction(mut self, ack_reaction: Option<AckReactionConfig>) -> Self {
        self.ack_reaction = ack_reaction;
        self
    }

    /// Configure workspace directory used for validating local attachment paths.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.discord")
    }

    /// Check if a Discord user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn is_group_sender_trigger_enabled(&self, sender_id: &str) -> bool {
        let sender_id = sender_id.trim();
        if sender_id.is_empty() {
            return false;
        }
        self.group_reply_allowed_sender_ids
            .iter()
            .any(|entry| entry == "*" || entry == sender_id)
    }

    fn bot_user_id_from_token(token: &str) -> Option<String> {
        // Discord bot tokens are base64(bot_user_id).timestamp.hmac
        let part = token.split('.').next()?;
        base64_decode(part)
    }

    fn resolve_local_attachment_path(&self, target: &str) -> anyhow::Result<PathBuf> {
        let workspace = self.workspace_dir.as_ref().ok_or_else(|| {
            anyhow::anyhow!("workspace_dir is not configured; local file attachments are disabled")
        })?;
        let workspace_root = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());

        let target_path = if let Some(rel) = target.strip_prefix("/workspace/") {
            workspace.join(rel)
        } else if target == "/workspace" {
            workspace.to_path_buf()
        } else {
            let path = Path::new(target);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace.join(path)
            }
        };

        let resolved = target_path
            .canonicalize()
            .with_context(|| format!("attachment path not found: {target}"))?;

        if !resolved.starts_with(&workspace_root) {
            anyhow::bail!("attachment path escapes workspace: {target}");
        }

        if !resolved.is_file() {
            anyhow::bail!("attachment path is not a file: {}", resolved.display());
        }

        Ok(resolved)
    }
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

/// Process Discord message attachments and return a string to append to the
/// agent message context.
///
/// `image/*` attachments are forwarded as `[IMAGE:<url>]` markers. For
/// `application/octet-stream` or missing MIME types, image-like filename/url
/// extensions are also treated as images.
/// `text/*` MIME types are fetched and inlined. Other types are skipped.
/// Fetch errors are logged as warnings.
async fn process_attachments(
    attachments: &[serde_json::Value],
    client: &reqwest::Client,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for att in attachments {
        let ct = att
            .get("content_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = att
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("file");
        let Some(url) = att.get("url").and_then(|v| v.as_str()) else {
            tracing::warn!(name, "discord: attachment has no url, skipping");
            continue;
        };
        if is_image_attachment(ct, name, url) {
            parts.push(format!("[IMAGE:{url}]"));
        } else if ct.starts_with("text/") {
            match client.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(text) = resp.text().await {
                        parts.push(format!("[{name}]\n{text}"));
                    }
                }
                Ok(resp) => {
                    tracing::warn!(name, status = %resp.status(), "discord attachment fetch failed");
                }
                Err(e) => {
                    tracing::warn!(name, error = %e, "discord attachment fetch error");
                }
            }
        } else {
            tracing::debug!(
                name,
                content_type = ct,
                "discord: skipping unsupported attachment type"
            );
        }
    }
    parts.join("\n---\n")
}

fn is_image_attachment(content_type: &str, filename: &str, url: &str) -> bool {
    let normalized_content_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if !normalized_content_type.is_empty() {
        if normalized_content_type.starts_with("image/") {
            return true;
        }
        // Trust explicit non-image MIME to avoid false positives from filename extensions.
        if normalized_content_type != "application/octet-stream" {
            return false;
        }
    }

    has_image_extension(filename) || has_image_extension(url)
}

fn has_image_extension(value: &str) -> bool {
    let base = value.split('?').next().unwrap_or(value);
    let base = base.split('#').next().unwrap_or(base);
    let ext = Path::new(base)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    matches!(
        ext.as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "bmp"
                | "tif"
                | "tiff"
                | "svg"
                | "avif"
                | "heic"
                | "heif"
        )
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscordAttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

impl DiscordAttachmentKind {
    fn from_marker(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_uppercase().as_str() {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }

    fn marker_name(&self) -> &'static str {
        match self {
            Self::Image => "IMAGE",
            Self::Document => "DOCUMENT",
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
            Self::Voice => "VOICE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscordAttachment {
    kind: DiscordAttachmentKind,
    target: String,
}

fn parse_attachment_markers(message: &str) -> (String, Vec<DiscordAttachment>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut attachments = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = message[cursor..].find('[') {
        let start = cursor + rel_start;
        cleaned.push_str(&message[cursor..start]);

        let Some(rel_end) = message[start..].find(']') else {
            cleaned.push_str(&message[start..]);
            cursor = message.len();
            break;
        };
        let end = start + rel_end;
        let marker_text = &message[start + 1..end];

        let parsed = marker_text.split_once(':').and_then(|(kind, target)| {
            let kind = DiscordAttachmentKind::from_marker(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(DiscordAttachment {
                kind,
                target: target.to_string(),
            })
        });

        if let Some(attachment) = parsed {
            attachments.push(attachment);
        } else {
            cleaned.push_str(&message[start..=end]);
        }

        cursor = end + 1;
    }

    if cursor < message.len() {
        cleaned.push_str(&message[cursor..]);
    }

    (cleaned.trim().to_string(), attachments)
}

fn classify_outgoing_attachments(
    attachments: &[DiscordAttachment],
) -> (Vec<DiscordAttachment>, Vec<String>, Vec<String>) {
    let mut local_files = Vec::new();
    let mut remote_urls = Vec::new();
    let unresolved_markers = Vec::new();

    for attachment in attachments {
        let target = attachment.target.trim();
        if target.starts_with("https://") || target.starts_with("http://") {
            remote_urls.push(target.to_string());
            continue;
        }

        local_files.push(attachment.clone());
    }

    (local_files, remote_urls, unresolved_markers)
}

fn with_inline_attachment_urls(
    content: &str,
    remote_urls: &[String],
    unresolved_markers: &[String],
) -> String {
    let mut lines = Vec::new();
    if !content.trim().is_empty() {
        lines.push(content.trim().to_string());
    }
    if !remote_urls.is_empty() {
        lines.extend(remote_urls.iter().cloned());
    }
    if !unresolved_markers.is_empty() {
        lines.extend(unresolved_markers.iter().cloned());
    }
    lines.join("\n")
}

async fn send_discord_message_json(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    content: &str,
) -> anyhow::Result<()> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");
    let body = json!({ "content": content });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        let sanitized = crate::providers::sanitize_api_error(&err);
        anyhow::bail!("Discord send message failed ({status}): {sanitized}");
    }

    Ok(())
}

async fn send_discord_message_with_files(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    content: &str,
    files: &[PathBuf],
) -> anyhow::Result<()> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");

    let mut form = Form::new().text("payload_json", json!({ "content": content }).to_string());

    for (idx, path) in files.iter().enumerate() {
        let bytes = tokio::fs::read(path).await.map_err(|error| {
            anyhow::anyhow!(
                "Discord attachment read failed for '{}': {error}",
                path.display()
            )
        })?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment.bin")
            .to_string();
        form = form.part(
            format!("files[{idx}]"),
            Part::bytes(bytes).file_name(filename),
        );
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .multipart(form)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        let sanitized = crate::providers::sanitize_api_error(&err);
        anyhow::bail!("Discord send message with files failed ({status}): {sanitized}");
    }

    Ok(())
}

const BASE64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Discord's maximum message length for regular messages.
///
/// Discord rejects longer payloads with `50035 Invalid Form Body`.
const DISCORD_MAX_MESSAGE_LENGTH: usize = 2000;
const DISCORD_ACK_REACTIONS: &[&str] = &["⚡️", "🦀", "🙌", "💪", "👌", "👀", "👣"];

/// Split a message into chunks that respect Discord's 2000-character limit.
/// Tries to split at word boundaries when possible.
fn split_message_for_discord(message: &str) -> Vec<String> {
    if message.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;

    while !remaining.is_empty() {
        // Find the byte offset for the 2000th character boundary.
        // If there are fewer than 2000 chars left, we can emit the tail directly.
        let hard_split = remaining
            .char_indices()
            .nth(DISCORD_MAX_MESSAGE_LENGTH)
            .map_or(remaining.len(), |(idx, _)| idx);

        let chunk_end = if hard_split == remaining.len() {
            hard_split
        } else {
            // Try to find a good break point (newline, then space)
            let search_area = &remaining[..hard_split];

            // Prefer splitting at newline
            if let Some(pos) = search_area.rfind('\n') {
                // Don't split if the newline is too close to the end
                if search_area[..pos].chars().count() >= DISCORD_MAX_MESSAGE_LENGTH / 2 {
                    pos + 1
                } else {
                    // Try space as fallback
                    search_area.rfind(' ').map_or(hard_split, |space| space + 1)
                }
            } else if let Some(pos) = search_area.rfind(' ') {
                pos + 1
            } else {
                // Hard split at the limit
                hard_split
            }
        };

        chunks.push(remaining[..chunk_end].to_string());
        remaining = &remaining[chunk_end..];
    }

    chunks
}

#[allow(clippy::cast_possible_truncation)]
fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let upper = len as u64;
    let reject_threshold = (u64::MAX / upper) * upper;

    loop {
        let value = rand::random::<u64>();
        if value < reject_threshold {
            return (value % upper) as usize;
        }
    }
}

fn random_discord_ack_reaction() -> &'static str {
    DISCORD_ACK_REACTIONS[pick_uniform_index(DISCORD_ACK_REACTIONS.len())]
}

/// URL-encode a Unicode emoji for use in Discord reaction API paths.
///
/// Discord's reaction endpoints accept raw Unicode emoji in the URL path,
/// but they must be percent-encoded per RFC 3986. Custom guild emojis use
/// the `name:id` format and are passed through unencoded.
fn encode_emoji_for_discord(emoji: &str) -> String {
    if emoji.contains(':') {
        return emoji.to_string();
    }

    use std::fmt::Write as _;
    let mut encoded = String::new();
    for byte in emoji.as_bytes() {
        write!(encoded, "%{byte:02X}").ok();
    }
    encoded
}

fn discord_reaction_url(channel_id: &str, message_id: &str, emoji: &str) -> String {
    let raw_id = message_id.strip_prefix("discord_").unwrap_or(message_id);
    let encoded_emoji = encode_emoji_for_discord(emoji);
    format!(
        "https://discord.com/api/v10/channels/{channel_id}/messages/{raw_id}/reactions/{encoded_emoji}/@me"
    )
}

fn mention_tags(bot_user_id: &str) -> [String; 2] {
    [format!("<@{bot_user_id}>"), format!("<@!{bot_user_id}>")]
}

fn contains_bot_mention(content: &str, bot_user_id: &str) -> bool {
    let tags = mention_tags(bot_user_id);
    content.contains(&tags[0]) || content.contains(&tags[1])
}

fn normalize_incoming_content(
    content: &str,
    require_mention: bool,
    bot_user_id: &str,
) -> Option<String> {
    if content.is_empty() {
        return None;
    }

    if require_mention && !contains_bot_mention(content, bot_user_id) {
        return None;
    }

    let mut normalized = content.to_string();
    if require_mention {
        for tag in mention_tags(bot_user_id) {
            normalized = normalized.replace(&tag, " ");
        }
    }

    let normalized = normalized.trim().to_string();
    if normalized.is_empty() {
        return None;
    }

    Some(normalized)
}

fn parse_approval_request_id(custom_id: &str, prefix: &str) -> Option<String> {
    let raw = custom_id.strip_prefix(prefix)?.trim();
    if raw.is_empty() || raw.chars().any(char::is_whitespace) {
        return None;
    }
    Some(raw.to_string())
}

/// Parse a Discord `INTERACTION_CREATE` message-component event into a
/// slash-command-equivalent ChannelMessage.
fn try_parse_approval_interaction(
    d: &serde_json::Value,
) -> Option<(ChannelMessage, String, String)> {
    // type=3 => MessageComponent interaction
    let interaction_type = d.get("type").and_then(serde_json::Value::as_u64)?;
    if interaction_type != 3 {
        return None;
    }

    let interaction_id = d.get("id").and_then(serde_json::Value::as_str)?.to_string();
    let interaction_token = d
        .get("token")
        .and_then(serde_json::Value::as_str)?
        .to_string();

    let custom_id = d
        .get("data")
        .and_then(|data| data.get("custom_id"))
        .and_then(serde_json::Value::as_str)?;

    let content = if let Some(request_id) =
        parse_approval_request_id(custom_id, DISCORD_APPROVAL_APPROVE_PREFIX)
    {
        format!("/approve-allow {request_id}")
    } else if let Some(request_id) =
        parse_approval_request_id(custom_id, DISCORD_APPROVAL_DENY_PREFIX)
    {
        format!("/approve-deny {request_id}")
    } else {
        return None;
    };

    // Guild interactions expose user in member.user; DMs expose top-level user.
    let user = d
        .get("member")
        .and_then(|member| member.get("user"))
        .or_else(|| d.get("user"))?;
    let user_id = user
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");

    let channel_id = d
        .get("channel_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let message = ChannelMessage {
        id: format!("discord_interaction_{interaction_id}"),
        sender: user_id.to_string(),
        reply_target: if channel_id.is_empty() {
            user_id.to_string()
        } else {
            channel_id.to_string()
        },
        content,
        channel: "discord".to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        thread_ts: None,
    };

    Some((message, interaction_id, interaction_token))
}

/// ACK an interaction by editing the original message and removing its buttons.
fn acknowledge_interaction_nonblocking(
    client: reqwest::Client,
    interaction_id: String,
    interaction_token: String,
    approved: bool,
) {
    let decision_text = if approved { "Approved" } else { "Denied" };
    let emoji = if approved { "\u{2705}" } else { "\u{274c}" };

    tokio::spawn(async move {
        let url = format!(
            "https://discord.com/api/v10/interactions/{interaction_id}/{interaction_token}/callback"
        );
        let body = json!({
            "type": 7,
            "data": {
                "content": format!("{emoji} {decision_text}."),
                "components": []
            }
        });
        let _ = client.post(&url).json(&body).send().await;
    });
}

/// Minimal base64 decode (no extra dep) — only needs to decode the user ID portion
#[allow(clippy::cast_possible_truncation)]
fn base64_decode(input: &str) -> Option<String> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };

    let mut bytes = Vec::new();
    let chars: Vec<u8> = padded.bytes().collect();

    for chunk in chars.chunks(4) {
        if chunk.len() < 4 {
            break;
        }

        let mut v = [0usize; 4];
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                v[i] = 0;
            } else {
                v[i] = BASE64_ALPHABET.iter().position(|&a| a == b)?;
            }
        }

        bytes.push(((v[0] << 2) | (v[1] >> 4)) as u8);
        if chunk[2] != b'=' {
            bytes.push((((v[1] & 0xF) << 4) | (v[2] >> 2)) as u8);
        }
        if chunk[3] != b'=' {
            bytes.push((((v[2] & 0x3) << 6) | v[3]) as u8);
        }
    }

    String::from_utf8(bytes).ok()
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let raw_content = super::strip_tool_call_tags(&message.content);
        let (cleaned_content, parsed_attachments) = parse_attachment_markers(&raw_content);
        let (local_attachment_targets, remote_urls, mut unresolved_markers) =
            classify_outgoing_attachments(&parsed_attachments);
        let mut local_files = Vec::new();

        for attachment in &local_attachment_targets {
            let target = attachment.target.trim();
            match self.resolve_local_attachment_path(target) {
                Ok(path) => local_files.push(path),
                Err(error) => {
                    tracing::warn!(
                        target,
                        error = %error,
                        "discord: local attachment rejected by workspace policy"
                    );
                    unresolved_markers.push(format!(
                        "[{}:{}]",
                        attachment.kind.marker_name(),
                        target
                    ));
                }
            }
        }

        if !unresolved_markers.is_empty() {
            tracing::warn!(
                unresolved = ?unresolved_markers,
                "discord: unresolved attachment markers were sent as plain text"
            );
        }

        // Discord accepts max 10 files per message.
        if local_files.len() > 10 {
            tracing::warn!(
                count = local_files.len(),
                "discord: truncating local attachment upload list to 10 files"
            );
            local_files.truncate(10);
        }

        let content =
            with_inline_attachment_urls(&cleaned_content, &remote_urls, &unresolved_markers);
        let chunks = split_message_for_discord(&content);
        let client = self.http_client();

        for (i, chunk) in chunks.iter().enumerate() {
            if i == 0 && !local_files.is_empty() {
                send_discord_message_with_files(
                    &client,
                    &self.bot_token,
                    &message.recipient,
                    chunk,
                    &local_files,
                )
                .await?;
            } else {
                send_discord_message_json(&client, &self.bot_token, &message.recipient, chunk)
                    .await?;
            }

            if i < chunks.len() - 1 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = Self::bot_user_id_from_token(&self.bot_token).unwrap_or_default();

        // Get Gateway URL
        let gw_resp: serde_json::Value = self
            .http_client()
            .get("https://discord.com/api/v10/gateway/bot")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await?
            .json()
            .await?;

        let gw_url = gw_resp
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or("wss://gateway.discord.gg");

        let ws_url = format!("{gw_url}/?v=10&encoding=json");
        tracing::info!("Discord: connecting to gateway...");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read.next().await.ok_or(anyhow::anyhow!("No hello"))??;
        let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())?;
        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        // Send Identify (opcode 2)
        let identify = json!({
            "op": 2,
            "d": {
                "token": self.bot_token,
                "intents": 37377, // GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT | DIRECT_MESSAGES
                "properties": {
                    "os": "linux",
                    "browser": "zeroclaw",
                    "device": "zeroclaw"
                }
            }
        });
        write
            .send(Message::Text(identify.to_string().into()))
            .await?;

        tracing::info!("Discord: connected and identified");

        // Track the last sequence number for heartbeats and resume.
        // Only accessed in the select! loop below, so a plain i64 suffices.
        let mut sequence: i64 = -1;

        // Spawn heartbeat timer — sends a tick signal, actual heartbeat
        // is assembled in the select! loop where `sequence` lives.
        let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
        let hb_interval = heartbeat_interval;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(hb_interval));
            loop {
                interval.tick().await;
                if hb_tx.send(()).await.is_err() {
                    break;
                }
            }
        });

        let guild_filter = self.guild_id.clone();

        loop {
            tokio::select! {
                _ = hb_rx.recv() => {
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                        break;
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(_))) | None => break,
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Track sequence number from all dispatch events
                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                    }

                    let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);

                    match op {
                        // Op 1: Server requests an immediate heartbeat
                        1 => {
                            let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                            let hb = json!({"op": 1, "d": d});
                            if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        // Op 7: Reconnect
                        7 => {
                            tracing::warn!("Discord: received Reconnect (op 7), closing for restart");
                            break;
                        }
                        // Op 9: Invalid Session
                        9 => {
                            tracing::warn!("Discord: received Invalid Session (op 9), closing for restart");
                            break;
                        }
                        _ => {}
                    }

                    let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");

                    // Handle button interaction callbacks for tool approvals.
                    if event_type == "INTERACTION_CREATE" {
                        if let Some(d) = event.get("d") {
                            if let Some((channel_msg, interaction_id, interaction_token)) =
                                try_parse_approval_interaction(d)
                            {
                                if !self.is_user_allowed(&channel_msg.sender) {
                                    tracing::warn!(
                                        "Discord: ignoring approval interaction from unauthorized user: {}",
                                        channel_msg.sender
                                    );
                                    // Always ACK to avoid "interaction failed" in Discord client.
                                    acknowledge_interaction_nonblocking(
                                        self.http_client(),
                                        interaction_id,
                                        interaction_token,
                                        false,
                                    );
                                    continue;
                                }

                                let approved = channel_msg.content.starts_with("/approve-allow ");
                                acknowledge_interaction_nonblocking(
                                    self.http_client(),
                                    interaction_id,
                                    interaction_token,
                                    approved,
                                );

                                if tx.send(channel_msg).await.is_err() {
                                    break;
                                }
                            }
                        }
                        continue;
                    }

                    if event_type != "MESSAGE_CREATE" {
                        continue;
                    }

                    let Some(d) = event.get("d") else {
                        continue;
                    };

                    // Skip messages from the bot itself
                    let author_id = d.get("author").and_then(|a| a.get("id")).and_then(|i| i.as_str()).unwrap_or("");
                    if author_id == bot_user_id {
                        continue;
                    }

                    // Skip bot messages (unless listen_to_bots is enabled)
                    if !self.listen_to_bots && d.get("author").and_then(|a| a.get("bot")).and_then(serde_json::Value::as_bool).unwrap_or(false) {
                        continue;
                    }

                    // Sender validation
                    if !self.is_user_allowed(author_id) {
                        tracing::warn!("Discord: ignoring message from unauthorized user: {author_id}");
                        continue;
                    }

                    // Guild filter
                    if let Some(ref gid) = guild_filter {
                        let msg_guild = d.get("guild_id").and_then(serde_json::Value::as_str);
                        // DMs have no guild_id — let them through; for guild messages, enforce the filter
                        if let Some(g) = msg_guild {
                            if g != gid {
                                continue;
                            }
                        }
                    }

                    let content = d.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let is_group_message = d.get("guild_id").is_some();
                    let allow_sender_without_mention =
                        is_group_message && self.is_group_sender_trigger_enabled(author_id);
                    let require_mention =
                        self.mention_only && is_group_message && !allow_sender_without_mention;
                    let Some(clean_content) =
                        normalize_incoming_content(content, require_mention, &bot_user_id)
                    else {
                        continue;
                    };

                    let attachment_text = {
                        let atts = d
                            .get("attachments")
                            .and_then(|a| a.as_array())
                            .cloned()
                            .unwrap_or_default();
                        process_attachments(&atts, &self.http_client()).await
                    };
                    let final_content = if attachment_text.is_empty() {
                        clean_content
                    } else {
                        format!("{clean_content}\n\n[Attachments]\n{attachment_text}")
                    };

                    let message_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let channel_id = d
                        .get("channel_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();

                    if !message_id.is_empty() && !channel_id.is_empty() {
                        let reaction_channel = DiscordChannel::new(
                            self.bot_token.clone(),
                            self.guild_id.clone(),
                            self.allowed_users.clone(),
                            self.listen_to_bots,
                            self.mention_only,
                        );
                        let reaction_channel_id = channel_id.clone();
                        let reaction_message_id = message_id.to_string();
                        let reaction_ctx = AckReactionContext {
                            text: &final_content,
                            sender_id: Some(author_id),
                            chat_id: Some(&channel_id),
                            chat_type: if is_group_message {
                                AckReactionContextChatType::Group
                            } else {
                                AckReactionContextChatType::Direct
                            },
                            locale_hint: None,
                        };
                        if let Some(reaction_emoji) = select_ack_reaction(
                            self.ack_reaction.as_ref(),
                            DISCORD_ACK_REACTIONS,
                            &reaction_ctx,
                        ) {
                            tokio::spawn(async move {
                                if let Err(err) = reaction_channel
                                    .add_reaction(
                                        &reaction_channel_id,
                                        &reaction_message_id,
                                        &reaction_emoji,
                                    )
                                    .await
                                {
                                    tracing::debug!(
                                        "Discord: failed to add ACK reaction for message {reaction_message_id}: {err}"
                                    );
                                }
                            });
                        }
                    }

                    let channel_msg = ChannelMessage {
                        id: if message_id.is_empty() {
                            Uuid::new_v4().to_string()
                        } else {
                            format!("discord_{message_id}")
                        },
                        sender: author_id.to_string(),
                        reply_target: if channel_id.is_empty() {
                            author_id.to_string()
                        } else {
                            channel_id.clone()
                        },
                        content: final_content,
                        channel: "discord".to_string(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: None,
                    };

                    if tx.send(channel_msg).await.is_err() {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn send_approval_prompt(
        &self,
        recipient: &str,
        request_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        _thread_ts: Option<String>,
    ) -> anyhow::Result<()> {
        let raw_args = arguments.to_string();
        let args_preview = if raw_args.chars().count() > 260 {
            crate::util::truncate_with_ellipsis(&raw_args, 260)
        } else {
            raw_args
        };

        let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");
        let body = json!({
            "content": format!(
                "**Approval required** for tool `{tool_name}`.\nRequest ID: `{request_id}`\nArgs: `{args_preview}`"
            ),
            "components": [{
                "type": 1,
                "components": [
                    {
                        "type": 2,
                        "style": 3,
                        "label": "Approve",
                        "custom_id": format!("{DISCORD_APPROVAL_APPROVE_PREFIX}{request_id}")
                    },
                    {
                        "type": 2,
                        "style": 4,
                        "label": "Deny",
                        "custom_id": format!("{DISCORD_APPROVAL_DENY_PREFIX}{request_id}")
                    }
                ]
            }]
        });

        let resp = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("Discord approval prompt failed ({status}): {sanitized}");
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get("https://discord.com/api/v10/users/@me")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.stop_typing(recipient).await?;

        let client = self.http_client();
        let token = self.bot_token.clone();
        let channel_id = recipient.to_string();

        let handle = tokio::spawn(async move {
            let url = format!("https://discord.com/api/v10/channels/{channel_id}/typing");
            loop {
                let _ = client
                    .post(&url)
                    .header("Authorization", format!("Bot {token}"))
                    .send()
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            }
        });

        let mut guard = self.typing_handles.lock();
        guard.insert(recipient.to_string(), handle);

        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let mut guard = self.typing_handles.lock();
        if let Some(handle) = guard.remove(recipient) {
            handle.abort();
        }
        Ok(())
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let url = discord_reaction_url(channel_id, message_id, emoji);

        let resp = self
            .http_client()
            .put(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .header("Content-Length", "0")
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("Discord add reaction failed ({status}): {sanitized}");
        }

        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let url = discord_reaction_url(channel_id, message_id, emoji);

        let resp = self
            .http_client()
            .delete(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("Discord remove reaction failed ({status}): {sanitized}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_channel_name() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        assert_eq!(ch.name(), "discord");
    }

    #[test]
    fn base64_decode_bot_id() {
        // "MTIzNDU2" decodes to "123456"
        let decoded = base64_decode("MTIzNDU2");
        assert_eq!(decoded, Some("123456".to_string()));
    }

    #[test]
    fn bot_user_id_extraction() {
        // Token format: base64(user_id).timestamp.hmac
        let token = "MTIzNDU2.fake.hmac";
        let id = DiscordChannel::bot_user_id_from_token(token);
        assert_eq!(id, Some("123456".to_string()));
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        assert!(!ch.is_user_allowed("12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["*".into()], false, false);
        assert!(ch.is_user_allowed("12345"));
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = DiscordChannel::new(
            "fake".into(),
            None,
            vec!["111".into(), "222".into()],
            false,
            false,
        );
        assert!(ch.is_user_allowed("111"));
        assert!(ch.is_user_allowed("222"));
        assert!(!ch.is_user_allowed("333"));
        assert!(!ch.is_user_allowed("unknown"));
    }

    #[test]
    fn allowlist_is_exact_match_not_substring() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["111".into()], false, false);
        assert!(!ch.is_user_allowed("1111"));
        assert!(!ch.is_user_allowed("11"));
        assert!(!ch.is_user_allowed("0111"));
    }

    #[test]
    fn allowlist_empty_string_user_id() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["111".into()], false, false);
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_with_wildcard_and_specific() {
        let ch = DiscordChannel::new(
            "fake".into(),
            None,
            vec!["111".into(), "*".into()],
            false,
            false,
        );
        assert!(ch.is_user_allowed("111"));
        assert!(ch.is_user_allowed("anyone_else"));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["ABC".into()], false, false);
        assert!(ch.is_user_allowed("ABC"));
        assert!(!ch.is_user_allowed("abc"));
        assert!(!ch.is_user_allowed("Abc"));
    }

    #[test]
    fn base64_decode_empty_string() {
        let decoded = base64_decode("");
        assert_eq!(decoded, Some(String::new()));
    }

    #[test]
    fn base64_decode_invalid_chars() {
        let decoded = base64_decode("!!!!");
        assert!(decoded.is_none());
    }

    #[test]
    fn bot_user_id_from_empty_token() {
        let id = DiscordChannel::bot_user_id_from_token("");
        assert_eq!(id, Some(String::new()));
    }

    #[test]
    fn contains_bot_mention_supports_plain_and_nick_forms() {
        assert!(contains_bot_mention("hi <@12345>", "12345"));
        assert!(contains_bot_mention("hi <@!12345>", "12345"));
        assert!(!contains_bot_mention("hi <@99999>", "12345"));
    }

    #[test]
    fn normalize_incoming_content_requires_mention_when_enabled() {
        let cleaned = normalize_incoming_content("hello there", true, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn normalize_incoming_content_strips_mentions_and_trims() {
        let cleaned = normalize_incoming_content("  <@!12345> run status  ", true, "12345");
        assert_eq!(cleaned.as_deref(), Some("run status"));
    }

    #[test]
    fn normalize_incoming_content_rejects_empty_after_strip() {
        let cleaned = normalize_incoming_content("<@12345>", true, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn normalize_group_reply_allowed_sender_ids_trims_and_deduplicates() {
        let normalized = normalize_group_reply_allowed_sender_ids(vec![
            " 111 ".into(),
            "111".into(),
            String::new(),
            "  ".into(),
            "222".into(),
        ]);
        assert_eq!(normalized, vec!["111".to_string(), "222".to_string()]);
    }

    #[test]
    fn group_reply_sender_override_matches_exact_and_wildcard() {
        let ch = DiscordChannel::new("token".into(), None, vec!["*".into()], false, true)
            .with_group_reply_allowed_senders(vec!["111".into(), "*".into()]);

        assert!(ch.is_group_sender_trigger_enabled("111"));
        assert!(ch.is_group_sender_trigger_enabled("anyone"));
        assert!(!ch.is_group_sender_trigger_enabled(""));
    }

    // Message splitting tests

    #[test]
    fn split_empty_message() {
        let chunks = split_message_for_discord("");
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn split_short_message_under_limit() {
        let msg = "Hello, world!";
        let chunks = split_message_for_discord(msg);
        assert_eq!(chunks, vec![msg]);
    }

    #[test]
    fn split_message_exactly_2000_chars() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_message_just_over_limit() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH + 1);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), 1);
    }

    #[test]
    fn split_very_long_message() {
        let msg = "word ".repeat(2000); // 10000 characters (5 chars per "word ")
        let chunks = split_message_for_discord(&msg);
        // Should split into 5 chunks of <= 2000 chars
        assert_eq!(chunks.len(), 5);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH));
        // Verify total content is preserved
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_prefer_newline_break() {
        let msg = format!("{}\n{}", "a".repeat(1500), "b".repeat(500));
        let chunks = split_message_for_discord(&msg);
        // Should split at the newline
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
        assert!(chunks[1].starts_with('b'));
    }

    #[test]
    fn split_prefer_space_break() {
        let msg = format!("{} {}", "a".repeat(1500), "b".repeat(600));
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn split_without_good_break_points_hard_split() {
        // No spaces or newlines - should hard split at 2000
        let msg = "a".repeat(5000);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[2].chars().count(), 1000);
    }

    #[test]
    fn split_multiple_breaks() {
        // Create a message with multiple newlines
        let part1 = "a".repeat(900);
        let part2 = "b".repeat(900);
        let part3 = "c".repeat(900);
        let msg = format!("{part1}\n{part2}\n{part3}");
        let chunks = split_message_for_discord(&msg);
        // Should split into 2 chunks (first two parts + third part)
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
        assert!(chunks[1].chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_preserves_content() {
        let original = "Hello world! This is a test message with some content. ".repeat(200);
        let chunks = split_message_for_discord(&original);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, original);
    }

    #[test]
    fn split_unicode_content() {
        // Test with emoji and multi-byte characters
        let msg = "🦀 Rust is awesome! ".repeat(500);
        let chunks = split_message_for_discord(&msg);
        // All chunks should be valid UTF-8
        for chunk in &chunks {
            assert!(std::str::from_utf8(chunk.as_bytes()).is_ok());
            assert!(chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
        }
        // Reconstruct and verify
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_newline_too_close_to_end() {
        // If newline is in the first half, don't use it - use space instead or hard split
        let msg = format!("{}\n{}", "a".repeat(1900), "b".repeat(500));
        let chunks = split_message_for_discord(&msg);
        // Should split at newline since it's in the second half of the window
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn split_multibyte_only_content_without_panics() {
        let msg = "🦀".repeat(2500);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), 500);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_chunks_always_within_discord_limit() {
        let msg = "x".repeat(12_345);
        let chunks = split_message_for_discord(&msg);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH));
    }

    #[test]
    fn split_message_with_multiple_newlines() {
        let msg = "Line 1\nLine 2\nLine 3\n".repeat(1000);
        let chunks = split_message_for_discord(&msg);
        assert!(chunks.len() > 1);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn typing_handles_start_empty() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let guard = ch.typing_handles.lock();
        assert!(guard.is_empty());
    }

    #[tokio::test]
    async fn start_typing_sets_handle() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let _ = ch.start_typing("123456").await;
        let guard = ch.typing_handles.lock();
        assert!(guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_clears_handle() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let _ = ch.start_typing("123456").await;
        let _ = ch.stop_typing("123456").await;
        let guard = ch.typing_handles.lock();
        assert!(!guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_is_idempotent() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        assert!(ch.stop_typing("123456").await.is_ok());
        assert!(ch.stop_typing("123456").await.is_ok());
    }

    #[tokio::test]
    async fn concurrent_typing_handles_are_independent() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let _ = ch.start_typing("111").await;
        let _ = ch.start_typing("222").await;
        {
            let guard = ch.typing_handles.lock();
            assert_eq!(guard.len(), 2);
            assert!(guard.contains_key("111"));
            assert!(guard.contains_key("222"));
        }
        // Stopping one does not affect the other
        let _ = ch.stop_typing("111").await;
        let guard = ch.typing_handles.lock();
        assert_eq!(guard.len(), 1);
        assert!(guard.contains_key("222"));
    }

    // ── Emoji encoding for reactions ──────────────────────────────

    #[test]
    fn encode_emoji_unicode_percent_encodes() {
        let encoded = encode_emoji_for_discord("\u{1F440}");
        assert_eq!(encoded, "%F0%9F%91%80");
    }

    #[test]
    fn encode_emoji_checkmark() {
        let encoded = encode_emoji_for_discord("\u{2705}");
        assert_eq!(encoded, "%E2%9C%85");
    }

    #[test]
    fn encode_emoji_custom_guild_emoji_passthrough() {
        let encoded = encode_emoji_for_discord("custom_emoji:123456789");
        assert_eq!(encoded, "custom_emoji:123456789");
    }

    #[test]
    fn encode_emoji_simple_ascii_char() {
        let encoded = encode_emoji_for_discord("A");
        assert_eq!(encoded, "%41");
    }

    #[test]
    fn random_discord_ack_reaction_is_from_pool() {
        for _ in 0..128 {
            let emoji = random_discord_ack_reaction();
            assert!(DISCORD_ACK_REACTIONS.contains(&emoji));
        }
    }

    #[test]
    fn discord_reaction_url_encodes_emoji_and_strips_prefix() {
        let url = discord_reaction_url("123", "discord_456", "👀");
        assert_eq!(
            url,
            "https://discord.com/api/v10/channels/123/messages/456/reactions/%F0%9F%91%80/@me"
        );
    }

    // ── Message ID edge cases ─────────────────────────────────────

    #[test]
    fn discord_message_id_format_includes_discord_prefix() {
        // Verify that message IDs follow the format: discord_{message_id}
        let message_id = "123456789012345678";
        let expected_id = format!("discord_{message_id}");
        assert_eq!(expected_id, "discord_123456789012345678");
    }

    #[test]
    fn discord_message_id_is_deterministic() {
        // Same message_id = same ID (prevents duplicates after restart)
        let message_id = "123456789012345678";
        let id1 = format!("discord_{message_id}");
        let id2 = format!("discord_{message_id}");
        assert_eq!(id1, id2);
    }

    #[test]
    fn discord_message_id_different_message_different_id() {
        // Different message IDs produce different IDs
        let id1 = "discord_123456789012345678".to_string();
        let id2 = "discord_987654321098765432".to_string();
        assert_ne!(id1, id2);
    }

    #[test]
    fn discord_message_id_uses_snowflake_id() {
        // Discord snowflake IDs are numeric strings
        let message_id = "123456789012345678"; // Typical snowflake format
        let id = format!("discord_{message_id}");
        assert!(id.starts_with("discord_"));
        // Snowflake IDs are numeric
        assert!(message_id.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn discord_message_id_fallback_to_uuid_on_empty() {
        // Edge case: empty message_id falls back to UUID
        let message_id = "";
        let id = if message_id.is_empty() {
            format!("discord_{}", uuid::Uuid::new_v4())
        } else {
            format!("discord_{message_id}")
        };
        assert!(id.starts_with("discord_"));
        // Should have UUID dashes
        assert!(id.contains('-'));
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG6: Channel platform limit edge cases for Discord (2000 char limit)
    // Prevents: Pattern 6 — issues #574, #499
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn split_message_code_block_at_boundary() {
        // Code block that spans the split boundary
        let mut msg = String::new();
        msg.push_str("```rust\n");
        msg.push_str(&"x".repeat(1990));
        msg.push_str("\n```\nMore text after code block");
        let parts = split_message_for_discord(&msg);
        assert!(
            parts.len() >= 2,
            "code block spanning boundary should split"
        );
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "each part must be <= {DISCORD_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
    }

    #[test]
    fn split_message_single_long_word_exceeds_limit() {
        // A single word longer than 2000 chars must be hard-split
        let long_word = "a".repeat(2500);
        let parts = split_message_for_discord(&long_word);
        assert!(parts.len() >= 2, "word exceeding limit must be split");
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "hard-split part must be <= {DISCORD_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
        // Reassembled content should match original
        let reassembled: String = parts.join("");
        assert_eq!(reassembled, long_word);
    }

    #[test]
    fn split_message_exactly_at_limit_no_split() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH);
        let parts = split_message_for_discord(&msg);
        assert_eq!(parts.len(), 1, "message exactly at limit should not split");
        assert_eq!(parts[0].len(), DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_message_one_over_limit_splits() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH + 1);
        let parts = split_message_for_discord(&msg);
        assert!(parts.len() >= 2, "message 1 char over limit must split");
    }

    #[test]
    #[allow(clippy::format_collect)]
    fn split_message_many_short_lines() {
        // Many short lines should be batched into chunks under the limit
        let msg: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "short-line batch must be <= limit"
            );
        }
        // All content should be preserved
        let reassembled: String = parts.join("");
        assert_eq!(reassembled.trim(), msg.trim());
    }

    #[test]
    fn split_message_only_whitespace() {
        let msg = "   \n\n\t  ";
        let parts = split_message_for_discord(msg);
        // Should handle gracefully without panic
        assert!(parts.len() <= 1);
    }

    #[test]
    fn split_message_emoji_at_boundary() {
        // Emoji are multi-byte; ensure we don't split mid-emoji
        let mut msg = "a".repeat(1998);
        msg.push_str("🎉🎊"); // 2 emoji at the boundary (2000 chars total)
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            // The function splits on character count, not byte count
            assert!(
                part.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH,
                "emoji boundary split must respect limit"
            );
        }
    }

    #[test]
    fn split_message_consecutive_newlines_at_boundary() {
        let mut msg = "a".repeat(1995);
        msg.push_str("\n\n\n\n\n");
        msg.push_str(&"b".repeat(100));
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            assert!(part.len() <= DISCORD_MAX_MESSAGE_LENGTH);
        }
    }

    // process_attachments tests

    #[tokio::test]
    async fn process_attachments_empty_list_returns_empty() {
        let client = reqwest::Client::new();
        let result = process_attachments(&[], &client).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn process_attachments_skips_unsupported_types() {
        let client = reqwest::Client::new();
        let attachments = vec![serde_json::json!({
            "url": "https://cdn.discordapp.com/attachments/123/456/doc.pdf",
            "filename": "doc.pdf",
            "content_type": "application/pdf"
        })];
        let result = process_attachments(&attachments, &client).await;
        assert!(result.is_empty());
    }

    async fn process_attachments_emits_image_marker_for_image_content_type() {
        let client = reqwest::Client::new();
        let attachments = vec![serde_json::json!({
            "url": "https://cdn.discordapp.com/attachments/123/456/photo.png",
            "filename": "photo.png",
            "content_type": "image/png"
        })];
        let result = process_attachments(&attachments, &client).await;
        assert_eq!(
            result,
            "[IMAGE:https://cdn.discordapp.com/attachments/123/456/photo.png]"
        );
    }

    #[tokio::test]
    async fn process_attachments_emits_multiple_image_markers() {
        let client = reqwest::Client::new();
        let attachments = vec![
            serde_json::json!({
                "url": "https://cdn.discordapp.com/attachments/123/456/one.jpg",
                "filename": "one.jpg",
                "content_type": "image/jpeg"
            }),
            serde_json::json!({
                "url": "https://cdn.discordapp.com/attachments/123/456/two.webp",
                "filename": "two.webp",
                "content_type": "image/webp"
            }),
        ];
        let result = process_attachments(&attachments, &client).await;
        assert_eq!(
            result,
            "[IMAGE:https://cdn.discordapp.com/attachments/123/456/one.jpg]\n---\n[IMAGE:https://cdn.discordapp.com/attachments/123/456/two.webp]"
        );
    }

    #[tokio::test]
    async fn process_attachments_emits_image_marker_from_filename_without_content_type() {
        let client = reqwest::Client::new();
        let attachments = vec![serde_json::json!({
            "url": "https://cdn.discordapp.com/attachments/123/456/photo.jpeg?size=1024",
            "filename": "photo.jpeg"
        })];
        let result = process_attachments(&attachments, &client).await;
        assert_eq!(
            result,
            "[IMAGE:https://cdn.discordapp.com/attachments/123/456/photo.jpeg?size=1024]"
        );
    }

    #[test]
    fn is_image_attachment_prefers_non_image_content_type_over_extension() {
        assert!(!is_image_attachment(
            "text/plain",
            "photo.png",
            "https://cdn.discordapp.com/attachments/123/456/photo.png"
        ));
    }

    #[test]
    fn is_image_attachment_allows_octet_stream_extension_fallback() {
        assert!(is_image_attachment(
            "application/octet-stream",
            "photo.png",
            "https://cdn.discordapp.com/attachments/123/456/photo.png"
        ));
    }
    #[test]
    fn parse_attachment_markers_extracts_supported_markers() {
        let input = "Report\n[IMAGE:https://example.com/a.png]\n[DOCUMENT:/tmp/a.pdf]";
        let (cleaned, attachments) = parse_attachment_markers(input);

        assert_eq!(cleaned, "Report");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, DiscordAttachmentKind::Image);
        assert_eq!(attachments[0].target, "https://example.com/a.png");
        assert_eq!(attachments[1].kind, DiscordAttachmentKind::Document);
        assert_eq!(attachments[1].target, "/tmp/a.pdf");
    }

    #[test]
    fn parse_attachment_markers_keeps_invalid_marker_text() {
        let input = "Hello [NOT_A_MARKER:foo] world";
        let (cleaned, attachments) = parse_attachment_markers(input);

        assert_eq!(cleaned, input);
        assert!(attachments.is_empty());
    }

    #[test]
    fn classify_outgoing_attachments_splits_local_remote_and_unresolved() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("image.png");
        std::fs::write(&file_path, b"fake").expect("write fixture");

        let attachments = vec![
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: file_path.to_string_lossy().to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: "https://example.com/remote.png".to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Video,
                target: "/tmp/does-not-exist.mp4".to_string(),
            },
        ];

        let (locals, remotes, unresolved) = classify_outgoing_attachments(&attachments);
        assert_eq!(locals.len(), 2);
        assert_eq!(locals[0].target, file_path.to_string_lossy());
        assert_eq!(locals[1].target, "/tmp/does-not-exist.mp4");
        assert_eq!(remotes, vec!["https://example.com/remote.png".to_string()]);
        assert!(unresolved.is_empty());
    }

    #[test]
    fn with_inline_attachment_urls_appends_urls_and_unresolved_markers() {
        let content = "Done";
        let remote_urls = vec!["https://example.com/a.png".to_string()];
        let unresolved = vec!["[IMAGE:/tmp/missing.png]".to_string()];

        let rendered = with_inline_attachment_urls(content, &remote_urls, &unresolved);
        assert_eq!(
            rendered,
            "Done\nhttps://example.com/a.png\n[IMAGE:/tmp/missing.png]"
        );
    }

    #[test]
    fn with_workspace_dir_sets_field() {
        let channel = DiscordChannel::new("fake".into(), None, vec![], false, false)
            .with_workspace_dir(PathBuf::from("/tmp/discord-workspace"));
        assert_eq!(
            channel.workspace_dir.as_deref(),
            Some(Path::new("/tmp/discord-workspace"))
        );
    }

    #[test]
    fn resolve_local_attachment_path_blocks_workspace_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace should exist");

        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, b"secret").expect("fixture should be written");

        let channel = DiscordChannel::new("fake".into(), None, vec![], false, false)
            .with_workspace_dir(workspace.clone());

        let allowed_path = workspace.join("ok.txt");
        std::fs::write(&allowed_path, b"ok").expect("workspace fixture should be written");
        let allowed = channel
            .resolve_local_attachment_path("ok.txt")
            .expect("workspace file should be allowed");
        assert!(allowed.starts_with(workspace.canonicalize().unwrap_or(workspace)));

        let escaped = channel.resolve_local_attachment_path(outside.to_string_lossy().as_ref());
        assert!(escaped.is_err(), "path outside workspace must be rejected");
    }

    #[test]
    fn discord_parse_approval_interaction_approve() {
        let event = json!({
            "type": 3,
            "id": "111222333",
            "token": "fake_token",
            "data": { "custom_id": "zcapr:yes:req-42" },
            "member": { "user": { "id": "user_1" } },
            "channel_id": "chan_99"
        });

        let (msg, interaction_id, interaction_token) =
            try_parse_approval_interaction(&event).expect("approval interaction should parse");
        assert_eq!(msg.content, "/approve-allow req-42");
        assert_eq!(msg.sender, "user_1");
        assert_eq!(msg.reply_target, "chan_99");
        assert_eq!(msg.channel, "discord");
        assert!(msg.id.contains("111222333"));
        assert_eq!(interaction_id, "111222333");
        assert_eq!(interaction_token, "fake_token");
    }

    #[test]
    fn discord_parse_approval_interaction_deny() {
        let event = json!({
            "type": 3,
            "id": "444555666",
            "token": "tok",
            "data": { "custom_id": "zcapr:no:req-99" },
            "user": { "id": "dm_user" },
            "channel_id": ""
        });

        let (msg, _, _) =
            try_parse_approval_interaction(&event).expect("deny interaction should parse");
        assert_eq!(msg.content, "/approve-deny req-99");
        assert_eq!(msg.sender, "dm_user");
        assert_eq!(msg.reply_target, "dm_user");
    }

    #[test]
    fn discord_parse_approval_interaction_ignores_non_approval() {
        let event = json!({
            "type": 3,
            "id": "777",
            "token": "tok",
            "data": { "custom_id": "some_other_button" },
            "member": { "user": { "id": "user_1" } },
            "channel_id": "chan_1"
        });

        assert!(try_parse_approval_interaction(&event).is_none());
    }

    #[test]
    fn discord_parse_approval_interaction_ignores_non_component() {
        let event = json!({
            "type": 2,
            "id": "888",
            "token": "tok",
            "data": { "custom_id": "zcapr:yes:req-1" },
            "member": { "user": { "id": "user_1" } },
            "channel_id": "chan_1"
        });

        assert!(try_parse_approval_interaction(&event).is_none());
    }

    #[test]
    fn discord_parse_approval_interaction_rejects_whitespace_request_id() {
        let event = json!({
            "type": 3,
            "id": "999",
            "token": "tok",
            "data": { "custom_id": "zcapr:yes:req 1" },
            "member": { "user": { "id": "user_1" } },
            "channel_id": "chan_1"
        });

        assert!(try_parse_approval_interaction(&event).is_none());
    }
}
