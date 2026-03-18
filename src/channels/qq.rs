use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_AUTH_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

/// Maximum upload size for QQ media files (10 MB).
const QQ_MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum entries in the upload cache before eviction.
const UPLOAD_CACHE_CAPACITY: usize = 500;

/// Passive reply limit per msg_id per hour (QQ API restriction).
const REPLY_LIMIT: u32 = 4;

/// Passive reply tracking window in seconds (1 hour).
const REPLY_TTL_SECS: u64 = 3600;

/// Maximum entries in the reply tracker before cleanup.
const REPLY_TRACKER_CAPACITY: usize = 10_000;

/// QQ API media file types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QQMediaFileType {
    /// Image (png, jpg, gif, etc.)
    Image = 1,
    /// Video (mp4, mov, etc.)
    Video = 2,
    /// Voice — only natively supported formats (.wav, .mp3, .silk).
    /// Non-native audio formats degrade to `File` instead.
    /// Note: The TS openclaw-qqbot uses silk-wasm + ffmpeg for full format
    /// transcoding; Rust version avoids heavyweight dependencies and only
    /// passes through natively supported formats.
    Voice = 3,
    /// File (pdf, zip, or any non-native audio format)
    File = 4,
}

/// A parsed media attachment from `[TYPE:target]` markers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QQMediaAttachment {
    kind: QQMediaFileType,
    target: String,
}

/// A segment of outbound message content — either plain text or a media attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum QQSendSegment {
    Text(String),
    Media(QQMediaAttachment),
}

/// Response from QQ media upload API.
#[derive(Debug, Deserialize)]
struct QQUploadResponse {
    file_info: String,
    #[allow(dead_code)]
    file_uuid: Option<String>,
    ttl: Option<u64>,
}

/// Cached upload entry to avoid re-uploading the same file within TTL.
struct UploadCacheEntry {
    file_info: String,
    expires_at: u64,
}

/// Tracks passive reply count per msg_id for QQ API rate limiting.
struct ReplyRecord {
    count: u32,
    first_reply_at: u64,
}

fn ensure_https(url: &str) -> anyhow::Result<()> {
    if !url.starts_with("https://") {
        anyhow::bail!(
            "Refusing to transmit sensitive data over non-HTTPS URL: URL scheme must be https"
        );
    }
    Ok(())
}

/// Check whether a file extension is a natively supported QQ voice format.
fn is_native_voice_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "wav" | "mp3" | "silk")
}

/// Map a `[TYPE:target]` marker kind string to `QQMediaFileType`.
///
/// For AUDIO/VOICE types, the target's extension determines whether it's
/// sent as `Voice` (native formats only) or degrades to `File`.
fn marker_kind_to_qq_file_type(marker: &str, target: &str) -> Option<QQMediaFileType> {
    match marker.trim().to_ascii_uppercase().as_str() {
        "IMAGE" | "PHOTO" => Some(QQMediaFileType::Image),
        "DOCUMENT" | "FILE" => Some(QQMediaFileType::File),
        "VIDEO" => Some(QQMediaFileType::Video),
        "AUDIO" | "VOICE" => {
            let ext = Path::new(target.split('?').next().unwrap_or(target))
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if is_native_voice_ext(ext) {
                Some(QQMediaFileType::Voice)
            } else {
                Some(QQMediaFileType::File)
            }
        }
        _ => None,
    }
}

/// Find the matching closing bracket, handling nested brackets.
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

/// Parse `[TYPE:target]` attachment markers from message content.
///
/// Returns the cleaned text (markers removed) and a list of parsed attachments.
/// Uses the same bracket-matching logic as `telegram.rs::parse_attachment_markers`.
fn parse_qq_attachment_markers(content: &str) -> (String, Vec<QQMediaAttachment>) {
    let mut cleaned = String::with_capacity(content.len());
    let mut attachments = Vec::new();
    let mut cursor = 0;

    while cursor < content.len() {
        let Some(open_rel) = content[cursor..].find('[') else {
            cleaned.push_str(&content[cursor..]);
            break;
        };

        let open = cursor + open_rel;
        cleaned.push_str(&content[cursor..open]);

        let Some(close_rel) = find_matching_close(&content[open + 1..]) else {
            cleaned.push_str(&content[open..]);
            break;
        };

        let close = open + 1 + close_rel;
        let marker = &content[open + 1..close];

        let parsed = marker.split_once(':').and_then(|(kind, target)| {
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            let file_type = marker_kind_to_qq_file_type(kind, target)?;
            Some(QQMediaAttachment {
                kind: file_type,
                target: target.to_string(),
            })
        });

        if let Some(attachment) = parsed {
            attachments.push(attachment);
        } else {
            cleaned.push_str(&content[open..=close]);
        }

        cursor = close + 1;
    }

    (cleaned.trim().to_string(), attachments)
}

/// Infer attachment type marker from content_type or filename.
fn infer_attachment_marker(content_type: &str, filename: &str) -> &'static str {
    let ct = content_type.to_ascii_lowercase();
    if ct.starts_with("image/") {
        return "IMAGE";
    }
    if ct.starts_with("audio/") || ct.contains("voice") {
        return "VOICE";
    }
    if ct.starts_with("video/") {
        return "VIDEO";
    }

    // Fallback to extension
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".heic")
        || lower.ends_with(".heif")
        || lower.ends_with(".svg")
    {
        return "IMAGE";
    }
    if lower.ends_with(".mp3")
        || lower.ends_with(".wav")
        || lower.ends_with(".silk")
        || lower.ends_with(".ogg")
        || lower.ends_with(".flac")
        || lower.ends_with(".m4a")
    {
        return "VOICE";
    }
    if lower.ends_with(".mp4")
        || lower.ends_with(".mov")
        || lower.ends_with(".mkv")
        || lower.ends_with(".avi")
        || lower.ends_with(".webm")
    {
        return "VIDEO";
    }
    "DOCUMENT"
}

/// Fix QQ attachment URLs that start with `//` (missing scheme).
fn fix_qq_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// Generate a message sequence number for QQ API requests.
/// Based on timestamp low bits XOR random, range 0~65535.
fn next_msg_seq() -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let time_part = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32)
        % 100_000_000;
    let random = u32::from(rand::random::<u16>());
    (time_part ^ random) % 65536
}

/// Get current unix timestamp in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Deduplication set capacity — evict half of entries when full.
const DEDUP_CAPACITY: usize = 10_000;

/// QQ Official Bot channel — uses Tencent's official QQ Bot API with
/// OAuth2 authentication and a Discord-like WebSocket gateway protocol.
pub struct QQChannel {
    app_id: String,
    app_secret: String,
    allowed_users: Vec<String>,
    /// Cached access token + expiry timestamp.
    token_cache: Arc<RwLock<Option<(String, u64)>>>,
    /// Message deduplication set.
    dedup: Arc<RwLock<HashSet<String>>>,
    /// Workspace directory for saving downloaded attachments.
    workspace_dir: Option<PathBuf>,
    /// Upload cache: avoids re-uploading the same file within TTL.
    upload_cache: Arc<RwLock<HashMap<String, UploadCacheEntry>>>,
    /// Passive reply tracker for QQ API rate limiting.
    reply_tracker: Arc<RwLock<HashMap<String, ReplyRecord>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
}

impl QQChannel {
    pub fn new(app_id: String, app_secret: String, allowed_users: Vec<String>) -> Self {
        Self {
            app_id,
            app_secret,
            allowed_users,
            token_cache: Arc::new(RwLock::new(None)),
            dedup: Arc::new(RwLock::new(HashSet::new())),
            workspace_dir: None,
            upload_cache: Arc::new(RwLock::new(HashMap::new())),
            reply_tracker: Arc::new(RwLock::new(HashMap::new())),
            proxy_url: None,
        }
    }

    /// Configure workspace directory for saving downloaded attachments.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_channel_proxy_client("channel.qq", self.proxy_url.as_deref())
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Fetch an access token from QQ's OAuth2 endpoint.
    async fn fetch_access_token(&self) -> anyhow::Result<(String, u64)> {
        let body = json!({
            "appId": self.app_id,
            "clientSecret": self.app_secret,
        });

        let resp = self
            .http_client()
            .post(QQ_AUTH_URL)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("QQ token request failed ({status}): {err}");
        }

        let data: serde_json::Value = resp.json().await?;
        let token = data
            .get("access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing access_token in QQ response"))?
            .to_string();

        let expires_in = data
            .get("expires_in")
            .and_then(|e| e.as_str())
            .and_then(|e| e.parse::<u64>().ok())
            .unwrap_or(7200);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Expire 60 seconds early to avoid edge cases
        let expiry = now + expires_in.saturating_sub(60);

        Ok((token, expiry))
    }

    /// Get a valid access token, refreshing if expired.
    async fn get_token(&self) -> anyhow::Result<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        {
            let cache = self.token_cache.read().await;
            if let Some((ref token, expiry)) = *cache {
                if now < expiry {
                    return Ok(token.clone());
                }
            }
        }

        let (token, expiry) = self.fetch_access_token().await?;
        {
            let mut cache = self.token_cache.write().await;
            *cache = Some((token.clone(), expiry));
        }
        Ok(token)
    }

    /// Get the WebSocket gateway URL.
    async fn get_gateway_url(&self, token: &str) -> anyhow::Result<String> {
        let resp = self
            .http_client()
            .get(format!("{QQ_API_BASE}/gateway"))
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("QQ gateway request failed ({status}): {err}");
        }

        let data: serde_json::Value = resp.json().await?;
        let url = data
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing gateway URL in QQ response"))?
            .to_string();

        Ok(url)
    }

    /// Check and insert message ID for deduplication.
    async fn is_duplicate(&self, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }

        let mut dedup = self.dedup.write().await;

        if dedup.contains(msg_id) {
            return true;
        }

        // Evict oldest half when at capacity
        if dedup.len() >= DEDUP_CAPACITY {
            let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
            for key in to_remove {
                dedup.remove(&key);
            }
        }

        dedup.insert(msg_id.to_string());
        false
    }

    /// Build upload cache key from file content hash.
    fn upload_cache_key(
        file_data: &[u8],
        scope: &str,
        target_id: &str,
        file_type: QQMediaFileType,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(file_data);
        let hash = format!("{:x}", hasher.finalize());
        format!("{hash}:{scope}:{target_id}:{}", file_type as u8)
    }

    /// Look up a cached file_info, returning it if still valid.
    async fn get_cached_upload(&self, cache_key: &str) -> Option<String> {
        let cache = self.upload_cache.read().await;
        if let Some(entry) = cache.get(cache_key) {
            // TTL safety margin: expire 60s early (same as TS version)
            if now_secs() + 60 < entry.expires_at {
                return Some(entry.file_info.clone());
            }
        }
        None
    }

    /// Store a file_info in the upload cache with TTL.
    async fn set_cached_upload(&self, cache_key: String, file_info: String, ttl: u64) {
        let mut cache = self.upload_cache.write().await;

        // Evict expired entries if at capacity
        if cache.len() >= UPLOAD_CACHE_CAPACITY {
            let now = now_secs();
            cache.retain(|_, v| v.expires_at > now);

            // If still at capacity, evict half
            if cache.len() >= UPLOAD_CACHE_CAPACITY {
                let keys_to_remove: Vec<String> = cache
                    .keys()
                    .take(UPLOAD_CACHE_CAPACITY / 2)
                    .cloned()
                    .collect();
                for key in keys_to_remove {
                    cache.remove(&key);
                }
            }
        }

        cache.insert(
            cache_key,
            UploadCacheEntry {
                file_info,
                expires_at: now_secs() + ttl,
            },
        );
    }

    /// Track passive reply count for a msg_id. Returns true if reply is allowed.
    async fn check_reply_allowed(&self, msg_id: &str) -> bool {
        let now = now_secs();
        let mut tracker = self.reply_tracker.write().await;

        // Cleanup if tracker is too large
        if tracker.len() >= REPLY_TRACKER_CAPACITY {
            tracker.retain(|_, v| now - v.first_reply_at < REPLY_TTL_SECS);
        }

        if let Some(record) = tracker.get_mut(msg_id) {
            if now - record.first_reply_at >= REPLY_TTL_SECS {
                // Window expired, cannot use passive reply
                return false;
            }
            if record.count >= REPLY_LIMIT {
                return false;
            }
            record.count += 1;
            true
        } else {
            tracker.insert(
                msg_id.to_string(),
                ReplyRecord {
                    count: 1,
                    first_reply_at: now,
                },
            );
            true
        }
    }

    /// Resolve the API endpoint path components from a recipient string.
    /// Returns (scope, id) where scope is "groups" or "users".
    fn resolve_recipient(recipient: &str) -> (&str, String) {
        if let Some(group_id) = recipient.strip_prefix("group:") {
            ("groups", group_id.to_string())
        } else {
            let raw_uid = recipient.strip_prefix("user:").unwrap_or(recipient);
            let user_id: String = raw_uid
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            ("users", user_id)
        }
    }

    /// Upload media to QQ API and return file_info for sending.
    ///
    /// Supports two modes:
    /// - URL upload: pass `url = Some(...)`, `file_data = None`
    /// - Base64 upload: pass `file_data = Some(...)`, `url = None`
    async fn upload_media(
        &self,
        recipient: &str,
        file_type: QQMediaFileType,
        url: Option<&str>,
        file_data: Option<&str>,
        file_name: Option<&str>,
    ) -> anyhow::Result<(String, Option<u64>)> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);

        let api_url = format!("{QQ_API_BASE}/v2/{scope}/{id}/files");
        ensure_https(&api_url)?;

        let mut body = json!({
            "file_type": file_type as u8,
            "srv_send_msg": false,
        });

        if let Some(u) = url {
            body["url"] = json!(u);
        }
        if let Some(d) = file_data {
            body["file_data"] = json!(d);
        }
        // QQ API uses file_name for File type to display the filename in chat
        if file_type == QQMediaFileType::File {
            if let Some(name) = file_name {
                body["file_name"] = json!(name);
            }
        }

        let resp = self
            .http_client()
            .post(&api_url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("QQ upload media failed ({status}): {err}");
        }

        let upload_resp: QQUploadResponse = resp.json().await?;
        Ok((upload_resp.file_info, upload_resp.ttl))
    }

    /// Send a media message (msg_type=7) with an already-uploaded file_info.
    async fn send_media_message(&self, recipient: &str, file_info: &str) -> anyhow::Result<()> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);

        let url = format!("{QQ_API_BASE}/v2/{scope}/{id}/messages");
        ensure_https(&url)?;

        let body = json!({
            "msg_type": 7,
            "media": {
                "file_info": file_info,
            },
            "msg_seq": next_msg_seq(),
        });

        let resp = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("QQ send media message failed ({status}): {err}");
        }

        Ok(())
    }

    /// Send a single attachment: resolve local/URL, upload, then send.
    async fn send_attachment(
        &self,
        recipient: &str,
        attachment: &QQMediaAttachment,
    ) -> anyhow::Result<()> {
        let target = attachment.target.trim();

        // Extract filename from target path/URL for File type display
        let file_name = Path::new(target.split('?').next().unwrap_or(target))
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());

        if target.starts_with("http://") || target.starts_with("https://") {
            // URL upload — no caching (remote content may change)
            let (file_info, _ttl) = self
                .upload_media(
                    recipient,
                    attachment.kind,
                    Some(target),
                    None,
                    file_name.as_deref(),
                )
                .await?;
            self.send_media_message(recipient, &file_info).await?;
        } else {
            // Local file upload
            let path = Path::new(target);
            if !path.exists() {
                anyhow::bail!("QQ attachment path not found: {target}");
            }

            let metadata = tokio::fs::metadata(path).await?;
            if metadata.len() > QQ_MAX_UPLOAD_BYTES {
                anyhow::bail!(
                    "QQ attachment too large ({} bytes, max {}): {target}",
                    metadata.len(),
                    QQ_MAX_UPLOAD_BYTES
                );
            }

            let file_bytes = tokio::fs::read(path).await?;
            let (scope_label, target_id) = Self::resolve_recipient(recipient);
            let scope = if scope_label == "groups" {
                "group"
            } else {
                "c2c"
            };
            let cache_key = Self::upload_cache_key(&file_bytes, scope, &target_id, attachment.kind);

            // Check upload cache
            if let Some(cached_file_info) = self.get_cached_upload(&cache_key).await {
                tracing::debug!("QQ: using cached upload for {target}");
                self.send_media_message(recipient, &cached_file_info)
                    .await?;
                return Ok(());
            }

            let b64 = base64::engine::general_purpose::STANDARD.encode(&file_bytes);
            let (file_info, ttl) = self
                .upload_media(
                    recipient,
                    attachment.kind,
                    None,
                    Some(&b64),
                    file_name.as_deref(),
                )
                .await?;

            // Cache the upload result
            if let Some(ttl_secs) = ttl {
                self.set_cached_upload(cache_key, file_info.clone(), ttl_secs)
                    .await;
            }

            self.send_media_message(recipient, &file_info).await?;
        }

        Ok(())
    }

    /// Compose message content from an incoming QQ event payload.
    ///
    /// Handles all attachment types (not just images), downloads to workspace
    /// if configured, and generates appropriate `[TYPE:path]` markers.
    async fn compose_message_content(&self, payload: &serde_json::Value) -> Option<String> {
        let text = payload
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();

        let mut markers: Vec<String> = Vec::new();
        let mut voice_transcripts: Vec<String> = Vec::new();

        if let Some(attachments) = payload.get("attachments").and_then(|a| a.as_array()) {
            for att in attachments {
                let url = match att.get("url").and_then(|u| u.as_str()) {
                    Some(u) if !u.trim().is_empty() => fix_qq_url(u),
                    _ => continue,
                };

                let content_type = att
                    .get("content_type")
                    .and_then(|ct| ct.as_str())
                    .unwrap_or("");
                let filename = att
                    .get("filename")
                    .and_then(|f| f.as_str())
                    .unwrap_or("attachment");

                let marker_type = infer_attachment_marker(content_type, filename);

                // For voice attachments, prefer voice_wav_url (WAV format) over
                // the default url (AMR/SILK). QQ provides this for direct use
                // without transcoding. (aligned with openclaw-qqbot behavior)
                let is_voice = content_type == "voice"
                    || content_type.starts_with("audio/")
                    || marker_type == "VOICE";
                let (download_url, save_filename) = if is_voice {
                    if let Some(wav_url) = att
                        .get("voice_wav_url")
                        .and_then(|u| u.as_str())
                        .filter(|u| !u.trim().is_empty())
                    {
                        let fixed = fix_qq_url(wav_url);
                        // Extract filename from WAV URL path
                        let wav_name = Path::new(fixed.split('?').next().unwrap_or(&fixed))
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("voice.wav")
                            .to_string();
                        (fixed, wav_name)
                    } else {
                        (url.clone(), filename.to_string())
                    }
                } else {
                    (url.clone(), filename.to_string())
                };

                // Try to download to workspace
                let location = if let Some(ref ws) = self.workspace_dir {
                    let dir = ws.join("qq_files");
                    match self
                        .download_attachment(&download_url, &dir, &save_filename)
                        .await
                    {
                        Ok(local_path) => local_path.display().to_string(),
                        Err(e) => {
                            tracing::warn!("QQ: failed to download attachment: {e}");
                            url.clone()
                        }
                    }
                } else {
                    url.clone()
                };

                if is_voice {
                    // For voice: include ASR transcription text (aligned with
                    // openclaw-qqbot format: "[语音消息] transcribed text")
                    // Also keep the file path marker for future multimodal support
                    markers.push(format!("[{marker_type}:{location}]"));
                    if let Some(asr_text) = att
                        .get("asr_refer_text")
                        .and_then(|t| t.as_str())
                        .map(|t| t.trim())
                        .filter(|t| !t.is_empty())
                    {
                        voice_transcripts.push(asr_text.to_string());
                    }
                } else {
                    markers.push(format!("[{marker_type}:{location}]"));
                }
            }
        }

        // Voice ASR transcription uses angle brackets to distinguish from
        // [TYPE:target] media markers (which use square brackets)
        let voice_text = match voice_transcripts.len() {
            0 => String::new(),
            1 => format!(
                "<VOICE_TRANSCRIPTION>{}</VOICE_TRANSCRIPTION>",
                voice_transcripts[0]
            ),
            _ => voice_transcripts
                .iter()
                .enumerate()
                .map(|(i, t)| format!("<VOICE_TRANSCRIPTION_{i}>{t}</VOICE_TRANSCRIPTION_{i}>"))
                .collect::<Vec<_>>()
                .join("\n"),
        };

        let mut parts: Vec<&str> = Vec::new();
        if !text.is_empty() {
            parts.push(text);
        }
        if !voice_text.is_empty() {
            parts.push(&voice_text);
        }
        let markers_joined = markers.join("\n");
        if !markers_joined.is_empty() {
            parts.push(&markers_joined);
        }

        if parts.is_empty() {
            return None;
        }

        Some(parts.join("\n"))
    }

    /// Download an attachment to the local workspace directory.
    async fn download_attachment(
        &self,
        url: &str,
        dir: &Path,
        filename: &str,
    ) -> anyhow::Result<PathBuf> {
        tokio::fs::create_dir_all(dir).await?;

        // Generate a unique filename to avoid collisions
        let stem = Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let unique = &Uuid::new_v4().to_string()[..8];
        let safe_name = if ext.is_empty() {
            format!("{stem}_{unique}")
        } else {
            format!("{stem}_{unique}.{ext}")
        };

        let dest = dir.join(&safe_name);

        // QQ multimedia URLs carry rkey auth in query params — no Authorization header needed
        // (consistent with openclaw-qqbot's downloadFile implementation)
        let resp = self.http_client().get(url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("Download failed ({}): {url}", resp.status());
        }

        let bytes = resp.bytes().await?;
        tokio::fs::write(&dest, &bytes).await?;

        Ok(dest)
    }

    /// Send a markdown text message (msg_type=2).
    async fn send_text_markdown(&self, recipient: &str, content: &str) -> anyhow::Result<()> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);

        let url = format!("{QQ_API_BASE}/v2/{scope}/{id}/messages");
        ensure_https(&url)?;

        let body = json!({
            "markdown": {
                "content": content,
            },
            "msg_type": 2,
            "msg_seq": next_msg_seq(),
        });

        let resp = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("QQ send message failed ({status}): {err}");
        }

        Ok(())
    }
}

#[async_trait]
impl Channel for QQChannel {
    fn name(&self) -> &str {
        "qq"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let (cleaned_text, attachments) = parse_qq_attachment_markers(&message.content);

        if attachments.is_empty() {
            // No media markers — send as markdown (original path)
            return self
                .send_text_markdown(&message.recipient, &message.content)
                .await;
        }

        // Send cleaned text first (if non-empty)
        if !cleaned_text.is_empty() {
            self.send_text_markdown(&message.recipient, &cleaned_text)
                .await?;
        }

        // Send each media attachment
        for attachment in &attachments {
            if let Err(e) = self.send_attachment(&message.recipient, attachment).await {
                tracing::warn!(
                    target = attachment.target,
                    error = %e,
                    "QQ: failed to send media attachment; falling back to text"
                );
                // Degrade to text fallback
                let fallback = format!(
                    "{}: {}",
                    match attachment.kind {
                        QQMediaFileType::Image => "Image",
                        QQMediaFileType::Video => "Video",
                        QQMediaFileType::Voice => "Voice",
                        QQMediaFileType::File => "File",
                    },
                    attachment.target
                );
                self.send_text_markdown(&message.recipient, &fallback)
                    .await?;
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!("QQ: authenticating...");
        let token = self.get_token().await?;

        tracing::info!("QQ: fetching gateway URL...");
        let gw_url = self.get_gateway_url(&token).await?;

        tracing::info!("QQ: connecting to gateway WebSocket...");
        let (ws_stream, _) =
            crate::config::ws_connect_with_proxy(&gw_url, "channel.qq", self.proxy_url.as_deref())
                .await?;
        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read
            .next()
            .await
            .ok_or(anyhow::anyhow!("QQ: no hello frame"))??;
        let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())?;
        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        // Send Identify (opcode 2)
        // Intents: PUBLIC_GUILD_MESSAGES (1<<30) | C2C_MESSAGE_CREATE & GROUP_AT_MESSAGE_CREATE (1<<25)
        let intents: u64 = (1 << 25) | (1 << 30);
        let identify = json!({
            "op": 2,
            "d": {
                "token": format!("QQBot {token}"),
                "intents": intents,
                "properties": {
                    "os": "linux",
                    "browser": "zeroclaw",
                    "device": "zeroclaw",
                }
            }
        });
        write
            .send(Message::Text(identify.to_string().into()))
            .await?;

        tracing::info!("QQ: connected and identified");

        let mut sequence: i64 = -1;

        // Spawn heartbeat timer
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

        loop {
            tokio::select! {
                _ = hb_rx.recv() => {
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write
                        .send(Message::Text(hb.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Ping(payload))) => {
                            if write.send(Message::Pong(payload)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Track sequence number
                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                    }

                    let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);

                    match op {
                        // Server requests immediate heartbeat
                        1 => {
                            let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                            let hb = json!({"op": 1, "d": d});
                            if write
                                .send(Message::Text(hb.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                        // Reconnect
                        7 => {
                            tracing::warn!("QQ: received Reconnect (op 7)");
                            break;
                        }
                        // Invalid Session
                        9 => {
                            tracing::warn!("QQ: received Invalid Session (op 9)");
                            break;
                        }
                        _ => {}
                    }

                    // Only process dispatch events (op 0)
                    if op != 0 {
                        continue;
                    }

                    let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");
                    let d = match event.get("d") {
                        Some(d) => d,
                        None => continue,
                    };

                    tracing::debug!("QQ: event_type={event_type} payload={d}");

                    match event_type {
                        "C2C_MESSAGE_CREATE" => {
                            let msg_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            if self.is_duplicate(msg_id).await {
                                continue;
                            }

                            let Some(content) = self.compose_message_content(d).await else {
                                continue;
                            };

                            let author_id = d.get("author").and_then(|a| a.get("id")).and_then(|i| i.as_str()).unwrap_or("unknown");
                            // For QQ, user_openid is the identifier
                            let user_openid = d.get("author").and_then(|a| a.get("user_openid")).and_then(|u| u.as_str()).unwrap_or(author_id);

                            if !self.is_user_allowed(user_openid) {
                                tracing::warn!("QQ: ignoring C2C message from unauthorized user: {user_openid}");
                                continue;
                            }

                            let chat_id = format!("user:{user_openid}");

                            let channel_msg = ChannelMessage {
                                id: Uuid::new_v4().to_string(),
                                sender: user_openid.to_string(),
                                reply_target: chat_id,
                                content,
                                channel: "qq".to_string(),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                thread_ts: None,
                                interruption_scope_id: None,
                    attachments: vec![],
                                observe_group: false,
                            };

                            if tx.send(channel_msg).await.is_err() {
                                tracing::warn!("QQ: message channel closed");
                                break;
                            }
                        }
                        "GROUP_AT_MESSAGE_CREATE" => {
                            let msg_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            if self.is_duplicate(msg_id).await {
                                continue;
                            }

                            let Some(content) = self.compose_message_content(d).await else {
                                continue;
                            };

                            let author_id = d.get("author").and_then(|a| a.get("member_openid")).and_then(|m| m.as_str()).unwrap_or("unknown");

                            if !self.is_user_allowed(author_id) {
                                tracing::warn!("QQ: ignoring group message from unauthorized user: {author_id}");
                                continue;
                            }

                            let group_openid = d.get("group_openid").and_then(|g| g.as_str()).unwrap_or("unknown");
                            let chat_id = format!("group:{group_openid}");

                            let channel_msg = ChannelMessage {
                                id: Uuid::new_v4().to_string(),
                                sender: author_id.to_string(),
                                reply_target: chat_id,
                                content,
                                channel: "qq".to_string(),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                thread_ts: None,
                                interruption_scope_id: None,
                    attachments: vec![],
                                observe_group: false,
                            };

                            if tx.send(channel_msg).await.is_err() {
                                tracing::warn!("QQ: message channel closed");
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        anyhow::bail!("QQ WebSocket connection closed")
    }

    async fn health_check(&self) -> bool {
        self.fetch_access_token().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_channel() -> QQChannel {
        QQChannel::new("id".into(), "secret".into(), vec![])
    }

    #[test]
    fn test_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "qq");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec!["*".into()]);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec!["user123".into()]);
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = make_channel();
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[tokio::test]
    async fn test_dedup() {
        let ch = make_channel();
        assert!(!ch.is_duplicate("msg1").await);
        assert!(ch.is_duplicate("msg1").await);
        assert!(!ch.is_duplicate("msg2").await);
    }

    #[tokio::test]
    async fn test_dedup_empty_id() {
        let ch = make_channel();
        assert!(!ch.is_duplicate("").await);
        assert!(!ch.is_duplicate("").await);
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
app_id = "12345"
app_secret = "secret_abc"
allowed_users = ["user1"]
"#;
        let config: crate::config::schema::QQConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.app_id, "12345");
        assert_eq!(config.app_secret, "secret_abc");
        assert_eq!(config.allowed_users, vec!["user1"]);
    }

    // --- Marker parsing tests ---

    #[test]
    fn test_parse_qq_markers_single_image() {
        let (text, atts) = parse_qq_attachment_markers("Hello [IMAGE:/tmp/a.png] world");
        assert_eq!(text, "Hello  world");
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].kind, QQMediaFileType::Image);
        assert_eq!(atts[0].target, "/tmp/a.png");
    }

    #[test]
    fn test_parse_qq_markers_multiple() {
        let (text, atts) =
            parse_qq_attachment_markers("[IMAGE:/a.png] text [VIDEO:https://example.com/v.mp4]");
        assert_eq!(text, "text");
        assert_eq!(atts.len(), 2);
        assert_eq!(atts[0].kind, QQMediaFileType::Image);
        assert_eq!(atts[1].kind, QQMediaFileType::Video);
    }

    #[test]
    fn test_parse_qq_markers_no_markers() {
        let (text, atts) = parse_qq_attachment_markers("Just plain text");
        assert_eq!(text, "Just plain text");
        assert!(atts.is_empty());
    }

    #[test]
    fn test_parse_qq_markers_case_insensitive() {
        let (_, atts) = parse_qq_attachment_markers("[image:/a.png]");
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].kind, QQMediaFileType::Image);

        let (_, atts) = parse_qq_attachment_markers("[Image:/a.png]");
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].kind, QQMediaFileType::Image);
    }

    #[test]
    fn test_parse_qq_markers_invalid_preserved() {
        let (text, atts) = parse_qq_attachment_markers("Keep [UNKNOWN:foo] here");
        assert_eq!(text, "Keep [UNKNOWN:foo] here");
        assert!(atts.is_empty());
    }

    #[test]
    fn test_parse_qq_markers_mixed_text_and_markers() {
        let (text, atts) =
            parse_qq_attachment_markers("Before [DOCUMENT:/doc.pdf] middle [PHOTO:/p.jpg] after");
        assert_eq!(text, "Before  middle  after");
        assert_eq!(atts.len(), 2);
        assert_eq!(atts[0].kind, QQMediaFileType::File);
        assert_eq!(atts[0].target, "/doc.pdf");
        assert_eq!(atts[1].kind, QQMediaFileType::Image);
        assert_eq!(atts[1].target, "/p.jpg");
    }

    // --- marker_kind_to_qq_file_type tests ---

    #[test]
    fn test_marker_kind_image() {
        assert_eq!(
            marker_kind_to_qq_file_type("IMAGE", "/a.png"),
            Some(QQMediaFileType::Image)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("PHOTO", "/a.png"),
            Some(QQMediaFileType::Image)
        );
    }

    #[test]
    fn test_marker_kind_document() {
        assert_eq!(
            marker_kind_to_qq_file_type("DOCUMENT", "/a.pdf"),
            Some(QQMediaFileType::File)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("FILE", "/a.zip"),
            Some(QQMediaFileType::File)
        );
    }

    #[test]
    fn test_marker_kind_video() {
        assert_eq!(
            marker_kind_to_qq_file_type("VIDEO", "/v.mp4"),
            Some(QQMediaFileType::Video)
        );
    }

    #[test]
    fn test_marker_kind_voice_native() {
        assert_eq!(
            marker_kind_to_qq_file_type("VOICE", "/a.mp3"),
            Some(QQMediaFileType::Voice)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("AUDIO", "/a.wav"),
            Some(QQMediaFileType::Voice)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("VOICE", "/a.silk"),
            Some(QQMediaFileType::Voice)
        );
    }

    #[test]
    fn test_marker_kind_voice_non_native_degrades() {
        // .ogg is not a natively supported QQ voice format — degrades to File
        assert_eq!(
            marker_kind_to_qq_file_type("VOICE", "/a.ogg"),
            Some(QQMediaFileType::File)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("AUDIO", "/a.flac"),
            Some(QQMediaFileType::File)
        );
    }

    // --- Upload/send body construction tests ---

    #[test]
    fn test_upload_body_url() {
        let body = json!({
            "file_type": QQMediaFileType::Image as u8,
            "srv_send_msg": false,
            "url": "https://example.com/a.jpg",
        });
        assert_eq!(body["file_type"], 1);
        assert_eq!(body["srv_send_msg"], false);
        assert_eq!(body["url"], "https://example.com/a.jpg");
        assert!(body.get("file_data").is_none());
    }

    #[test]
    fn test_upload_body_base64() {
        let body = json!({
            "file_type": QQMediaFileType::File as u8,
            "srv_send_msg": false,
            "file_data": "dGVzdA==",
        });
        assert_eq!(body["file_type"], 4);
        assert_eq!(body["file_data"], "dGVzdA==");
        assert!(body.get("url").is_none());
    }

    #[test]
    fn test_send_media_body_msg_type_7() {
        let file_info = "some_file_info_string";
        let body = json!({
            "msg_type": 7,
            "media": {
                "file_info": file_info,
            },
            "msg_seq": 1,
        });
        assert_eq!(body["msg_type"], 7);
        assert_eq!(body["media"]["file_info"], file_info);
    }

    // --- compose_message_content tests (now async) ---

    #[tokio::test]
    async fn test_compose_message_content_text_only() {
        let ch = make_channel();
        let payload = json!({ "content": "  hello world  " });
        assert_eq!(
            ch.compose_message_content(&payload).await,
            Some("hello world".to_string())
        );
    }

    #[tokio::test]
    async fn test_compose_message_content_image_attachment() {
        let ch = make_channel();
        let payload = json!({
            "content": "   ",
            "attachments": [{
                "content_type": "image/jpg",
                "url": "https://cdn.example.com/a.jpg"
            }]
        });
        assert_eq!(
            ch.compose_message_content(&payload).await,
            Some("[IMAGE:https://cdn.example.com/a.jpg]".to_string())
        );
    }

    #[tokio::test]
    async fn test_compose_message_content_text_and_attachments() {
        let ch = make_channel();
        let payload = json!({
            "content": "Here is an image",
            "attachments": [
                { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
                { "filename": "b.jpeg", "url": "https://cdn.example.com/b.jpeg" }
            ]
        });
        assert_eq!(
            ch.compose_message_content(&payload).await,
            Some(
                "Here is an image\n[IMAGE:https://cdn.example.com/a.png]\n[IMAGE:https://cdn.example.com/b.jpeg]"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn test_compose_all_attachment_types() {
        let ch = make_channel();
        let payload = json!({
            "content": "",
            "attachments": [
                { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
                { "content_type": "audio/mpeg", "url": "https://cdn.example.com/b.mp3" },
                { "content_type": "video/mp4", "url": "https://cdn.example.com/c.mp4" },
                { "content_type": "application/pdf", "url": "https://cdn.example.com/d.pdf" }
            ]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("[IMAGE:"));
        assert!(result.contains("[VOICE:"));
        assert!(result.contains("[VIDEO:"));
        assert!(result.contains("[DOCUMENT:"));
    }

    #[tokio::test]
    async fn test_compose_fixes_double_slash_url() {
        let ch = make_channel();
        let payload = json!({
            "content": "",
            "attachments": [{
                "content_type": "image/png",
                "url": "//cdn.example.com/a.png"
            }]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("https://cdn.example.com/a.png"));
        // Ensure the raw `//` prefix was replaced with `https:`
        assert!(!result.starts_with("[IMAGE://"));
    }

    #[tokio::test]
    async fn test_compose_fallback_no_workspace() {
        // Without workspace_dir, attachments use URLs directly
        let ch = make_channel();
        let payload = json!({
            "content": "text",
            "attachments": [{
                "content_type": "application/pdf",
                "filename": "report.pdf",
                "url": "https://cdn.example.com/report.pdf"
            }]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("[DOCUMENT:https://cdn.example.com/report.pdf]"));
    }

    #[tokio::test]
    async fn test_compose_drops_empty_url() {
        let ch = make_channel();
        let payload = json!({
            "content": "   ",
            "attachments": [{
                "content_type": "image/png",
                "url": "   "
            }]
        });
        assert_eq!(ch.compose_message_content(&payload).await, None);
    }

    // --- Markdown send body test ---

    #[test]
    fn test_send_body_uses_markdown_msg_type() {
        let content = "**bold** and `code`";
        let body = json!({
            "markdown": { "content": content },
            "msg_type": 2,
        });
        assert_eq!(body["msg_type"], 2);
        assert_eq!(body["markdown"]["content"], content);
        assert!(
            body.get("content").is_none(),
            "top-level 'content' must not be present"
        );
    }

    // --- Helper function tests ---

    #[test]
    fn test_fix_qq_url() {
        assert_eq!(
            fix_qq_url("//cdn.example.com/a.png"),
            "https://cdn.example.com/a.png"
        );
        assert_eq!(
            fix_qq_url("https://cdn.example.com/a.png"),
            "https://cdn.example.com/a.png"
        );
    }

    #[test]
    fn test_next_msg_seq_range() {
        for _ in 0..100 {
            let seq = next_msg_seq();
            assert!(seq < 65536);
        }
    }

    #[test]
    fn test_resolve_recipient_group() {
        let (scope, id) = QQChannel::resolve_recipient("group:abc123");
        assert_eq!(scope, "groups");
        assert_eq!(id, "abc123");
    }

    #[test]
    fn test_resolve_recipient_user() {
        let (scope, id) = QQChannel::resolve_recipient("user:xyz789");
        assert_eq!(scope, "users");
        assert_eq!(id, "xyz789");
    }

    #[test]
    fn test_resolve_recipient_bare_id() {
        let (scope, id) = QQChannel::resolve_recipient("raw_id_123");
        assert_eq!(scope, "users");
        assert_eq!(id, "raw_id_123");
    }

    #[test]
    fn test_infer_attachment_marker() {
        assert_eq!(infer_attachment_marker("image/png", "a.png"), "IMAGE");
        assert_eq!(infer_attachment_marker("audio/mpeg", "a.mp3"), "VOICE");
        assert_eq!(infer_attachment_marker("video/mp4", "a.mp4"), "VIDEO");
        assert_eq!(
            infer_attachment_marker("application/pdf", "doc.pdf"),
            "DOCUMENT"
        );
        assert_eq!(infer_attachment_marker("", "photo.jpg"), "IMAGE");
        assert_eq!(infer_attachment_marker("", "song.mp3"), "VOICE");
        assert_eq!(infer_attachment_marker("", "clip.mp4"), "VIDEO");
        assert_eq!(infer_attachment_marker("", "unknown.xyz"), "DOCUMENT");
    }

    // --- Upload cache tests ---

    #[tokio::test]
    async fn test_upload_cache_hit_and_miss() {
        let ch = make_channel();
        let key = QQChannel::upload_cache_key(b"test_data", "c2c", "user1", QQMediaFileType::Image);

        // Miss
        assert!(ch.get_cached_upload(&key).await.is_none());

        // Set with long TTL
        ch.set_cached_upload(key.clone(), "cached_file_info".into(), 3600)
            .await;

        // Hit
        assert_eq!(
            ch.get_cached_upload(&key).await,
            Some("cached_file_info".to_string())
        );
    }

    #[tokio::test]
    async fn test_upload_cache_expired() {
        let ch = make_channel();
        let key = QQChannel::upload_cache_key(b"test_data", "group", "g1", QQMediaFileType::Video);

        // Set with 0 TTL (already expired considering 60s safety margin)
        ch.set_cached_upload(key.clone(), "old_info".into(), 0)
            .await;

        // Should miss due to expiry
        assert!(ch.get_cached_upload(&key).await.is_none());
    }

    // --- Reply tracker tests ---

    #[tokio::test]
    async fn test_reply_tracker_allows_up_to_limit() {
        let ch = make_channel();
        for _ in 0..REPLY_LIMIT {
            assert!(ch.check_reply_allowed("msg1").await);
        }
        // 5th reply should be denied
        assert!(!ch.check_reply_allowed("msg1").await);
    }

    #[tokio::test]
    async fn test_reply_tracker_independent_msg_ids() {
        let ch = make_channel();
        assert!(ch.check_reply_allowed("msg_a").await);
        assert!(ch.check_reply_allowed("msg_b").await);
    }
}
