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
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_AUTH_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

/// Maximum upload size for QQ media files (10 MB).
const QQ_MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum QQ audio size sent to transcription providers (25 MB).
const QQ_MAX_AUDIO_TRANSCRIPTION_BYTES: u64 = 25 * 1024 * 1024;

/// Maximum entries in the upload cache before eviction.
const UPLOAD_CACHE_CAPACITY: usize = 500;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct QQVoiceDedupPart {
    source_id: String,
    transcript: String,
}

struct QQComposedMessage {
    content: String,
    voice_dedup_parts: Vec<QQVoiceDedupPart>,
}

/// Response from QQ media upload API.
#[derive(Debug, Deserialize)]
struct QQUploadResponse {
    file_info: String,
    ttl: Option<u64>,
}

/// Cached upload entry to avoid re-uploading the same file within TTL.
struct UploadCacheEntry {
    file_info: String,
    expires_at: u64,
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

fn has_supported_transcription_extension(filename: &str) -> bool {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    matches!(
        ext.as_str(),
        "flac" | "mp3" | "mpeg" | "mpga" | "mp4" | "m4a" | "ogg" | "oga" | "opus" | "wav" | "webm"
    )
}

fn voice_wav_filename(url: &str) -> String {
    let filename = Path::new(url.split('?').next().unwrap_or(url))
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("voice");

    if has_supported_transcription_extension(filename) {
        filename.to_string()
    } else {
        format!("{filename}.wav")
    }
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

const VOICE_REDELIVERY_DEDUP_WINDOW_SECS: u64 = 5 * 60;

/// Maximum number of retry attempts when fetching the access token.
const AUTH_RETRY_MAX_ATTEMPTS: u32 = 4;

/// Initial backoff delay for auth token retry (in milliseconds).
const AUTH_RETRY_INITIAL_BACKOFF_MS: u64 = 500;

/// Maximum backoff delay for auth token retry (in milliseconds).
const AUTH_RETRY_MAX_BACKOFF_MS: u64 = 8_000;

/// QQ Official Bot channel — uses Tencent's official QQ Bot API with
/// OAuth2 authentication and a Discord-like WebSocket gateway protocol.
pub struct QQChannel {
    app_id: String,
    app_secret: String,
    /// The alias key under `[channels.qq.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Cached access token + expiry timestamp.
    token_cache: Arc<RwLock<Option<(String, u64)>>>,
    /// Message deduplication set.
    dedup: Arc<RwLock<HashSet<String>>>,
    /// Workspace directory for saving downloaded attachments.
    workspace_dir: Option<PathBuf>,
    /// Upload cache: avoids re-uploading the same file within TTL.
    upload_cache: Arc<RwLock<HashMap<String, UploadCacheEntry>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Optional voice transcription manager for QQ audio attachments that do
    /// not already carry QQ platform ASR text.
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
    /// Session ID from the last READY event, used for gateway resume (opcode 6).
    session_id: Arc<RwLock<Option<String>>>,
    /// Last sequence number received, used for gateway resume (opcode 6).
    last_sequence: Arc<RwLock<Option<i64>>>,
}

impl QQChannel {
    pub fn new(
        app_id: String,
        app_secret: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            app_id,
            app_secret,
            alias: alias.into(),
            peer_resolver,
            token_cache: Arc::new(RwLock::new(None)),
            dedup: Arc::new(RwLock::new(HashSet::new())),
            workspace_dir: None,
            upload_cache: Arc::new(RwLock::new(HashMap::new())),
            proxy_url: None,
            transcription_manager: None,
            session_id: Arc::new(RwLock::new(None)),
            last_sequence: Arc::new(RwLock::new(None)),
        }
    }

    /// Return the alias under `[channels.qq.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
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

    /// Configure voice transcription for QQ audio attachments.
    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }

        match super::transcription::TranscriptionManager::new(&config) {
            Ok(manager) => {
                let sole_provider = {
                    let providers = manager.available_providers();
                    if providers.len() == 1 {
                        Some(providers[0].to_string())
                    } else {
                        None
                    }
                };
                let manager = if let Some(provider) = sole_provider {
                    manager.with_agent_transcription_provider(provider)
                } else {
                    manager
                };
                self.transcription_manager = Some(Arc::new(manager));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "transcription manager init failed, QQ voice transcription disabled"
                );
            }
        }

        self
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client("channel.qq", self.proxy_url.as_deref())
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
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
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Missing access_token in QQ response"
                );
                anyhow::Error::msg("Missing access_token in QQ response")
            })?
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

    /// Fetch an access token with retry and exponential backoff.
    ///
    /// Transient failures (network errors, 5xx responses) during reconnection
    /// can cause the entire recovery loop to fail. This method retries up to
    /// `AUTH_RETRY_MAX_ATTEMPTS` times with exponential backoff + jitter so
    /// that a single transient error doesn't permanently break the reconnect
    /// flow.
    async fn fetch_access_token_with_retry(&self) -> anyhow::Result<(String, u64)> {
        let mut backoff_ms = AUTH_RETRY_INITIAL_BACKOFF_MS;
        let mut last_err = None;

        for attempt in 1..=AUTH_RETRY_MAX_ATTEMPTS {
            match self.fetch_access_token().await {
                Ok(result) => {
                    if attempt > 1 {
                        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"attempt": attempt, "AUTH_RETRY_MAX_ATTEMPTS": AUTH_RETRY_MAX_ATTEMPTS})), "getAppAccessToken succeeded on attempt /");
                    }
                    return Ok(result);
                }
                Err(e) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"attempt": attempt, "AUTH_RETRY_MAX_ATTEMPTS": AUTH_RETRY_MAX_ATTEMPTS, "e": e.to_string()})), "getAppAccessToken failed (attempt /)");
                    last_err = Some(e);

                    if attempt < AUTH_RETRY_MAX_ATTEMPTS {
                        // Add jitter: 75%-125% of base backoff
                        let jitter_factor = 0.75 + (rand::random::<f64>() * 0.5);
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let sleep_ms = (backoff_ms as f64 * jitter_factor) as u64;
                        tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(AUTH_RETRY_MAX_BACKOFF_MS);
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "getAppAccessToken",
                        "max_attempts": AUTH_RETRY_MAX_ATTEMPTS,
                    })),
                "qq: getAppAccessToken exhausted retries"
            );
            anyhow::Error::msg(format!(
                "getAppAccessToken failed after {AUTH_RETRY_MAX_ATTEMPTS} attempts"
            ))
        }))
    }

    /// Get a valid access token, refreshing if expired.
    async fn get_token(&self) -> anyhow::Result<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        {
            let cache = self.token_cache.read().await;
            if let Some((ref token, expiry)) = *cache
                && now < expiry
            {
                return Ok(token.clone());
            }
        }

        let (token, expiry) = self.fetch_access_token_with_retry().await?;
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
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Missing gateway URL in QQ response"
                );
                anyhow::Error::msg("Missing gateway URL in QQ response")
            })?
            .to_string();

        Ok(url)
    }

    async fn record_dedup_key(&self, key: String) -> bool {
        let mut dedup = self.dedup.write().await;

        if dedup.contains(&key) {
            return true;
        }

        Self::evict_dedup_if_full(&mut dedup);

        dedup.insert(key);
        false
    }

    fn evict_dedup_if_full(dedup: &mut HashSet<String>) {
        if dedup.len() < DEDUP_CAPACITY {
            return;
        }

        let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
        for key in to_remove {
            dedup.remove(&key);
        }
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

        Self::evict_dedup_if_full(&mut dedup);

        dedup.insert(msg_id.to_string());
        false
    }

    async fn record_gateway_sequence(&self, sequence: i64) {
        if sequence >= 0 {
            *self.last_sequence.write().await = Some(sequence);
        }
    }

    fn normalize_voice_transcript(transcript: &str) -> Option<String> {
        let normalized = transcript.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    }

    fn voice_redelivery_dedup_hash(
        event_type: &str,
        reply_target: &str,
        sender: &str,
        parts: &[QQVoiceDedupPart],
    ) -> Option<String> {
        if parts.is_empty() {
            return None;
        }

        let mut hasher = Sha256::new();
        hasher.update(event_type.as_bytes());
        hasher.update(b"\0");
        hasher.update(reply_target.as_bytes());
        hasher.update(b"\0");
        hasher.update(sender.as_bytes());
        for part in parts {
            hasher.update(b"\0");
            hasher.update(part.source_id.as_bytes());
            hasher.update(b"\0");
            hasher.update(part.transcript.as_bytes());
        }

        Some(format!("{:x}", hasher.finalize()))
    }

    fn voice_redelivery_dedup_key(bucket: u64, hash: &str) -> String {
        format!("qq_voice:{bucket}:{hash}")
    }

    async fn is_duplicate_voice_redelivery(
        &self,
        event_type: &str,
        reply_target: &str,
        sender: &str,
        parts: &[QQVoiceDedupPart],
    ) -> bool {
        let Some(hash) = Self::voice_redelivery_dedup_hash(event_type, reply_target, sender, parts)
        else {
            return false;
        };

        let bucket = now_secs() / VOICE_REDELIVERY_DEDUP_WINDOW_SECS;
        let key = Self::voice_redelivery_dedup_key(bucket, &hash);

        self.record_dedup_key(key).await
    }

    fn is_voice_attachment(content_type: &str, filename: &str) -> bool {
        let marker_type = infer_attachment_marker(content_type, filename);
        content_type == "voice" || content_type.starts_with("audio/") || marker_type == "VOICE"
    }

    fn normalized_voice_source_id(url: &str) -> String {
        let fixed = fix_qq_url(url);
        let end = fixed
            .char_indices()
            .find_map(|(idx, ch)| (ch == '?' || ch == '#').then_some(idx))
            .unwrap_or(fixed.len());
        fixed[..end].to_string()
    }

    fn voice_attachment_source_id(att: &serde_json::Value) -> Option<String> {
        let content_type = att
            .get("content_type")
            .and_then(|ct| ct.as_str())
            .unwrap_or("");
        let filename = att
            .get("filename")
            .and_then(|f| f.as_str())
            .unwrap_or("attachment");

        if !Self::is_voice_attachment(content_type, filename) {
            return None;
        }

        let source_url = att
            .get("voice_wav_url")
            .and_then(|u| u.as_str())
            .filter(|u| !u.trim().is_empty())
            .or_else(|| {
                att.get("url")
                    .and_then(|u| u.as_str())
                    .filter(|u| !u.trim().is_empty())
            })?;

        Some(format!(
            "{}:{}:{}",
            content_type.trim().to_ascii_lowercase(),
            filename.trim(),
            Self::normalized_voice_source_id(source_url)
        ))
    }

    fn platform_voice_dedup_parts(payload: &serde_json::Value) -> Vec<QQVoiceDedupPart> {
        let Some(attachments) = payload.get("attachments").and_then(|a| a.as_array()) else {
            return Vec::new();
        };

        attachments
            .iter()
            .filter_map(|att| {
                let source_id = Self::voice_attachment_source_id(att)?;
                let transcript = att
                    .get("asr_refer_text")
                    .and_then(|t| t.as_str())
                    .and_then(Self::normalize_voice_transcript)?;
                Some(QQVoiceDedupPart {
                    source_id,
                    transcript,
                })
            })
            .collect()
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
        if file_type == QQMediaFileType::File
            && let Some(name) = file_name
        {
            body["file_name"] = json!(name);
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

    /// Build the JSON body for a QQ send request, optionally attaching
    /// a passive-reply `msg_id` when the recipient is a group.
    ///
    /// QQ Bot v2 API requires the triggering inbound `msg_id` in group
    /// `/messages` request bodies so the server can classify the send as a
    /// passive reply. Without it, the request is treated as an "active
    /// message" and rejected with code `40034102` when the bot only holds
    /// passive-reply permissions (the default for newly provisioned bots).
    /// See #7872.
    ///
    /// The id MUST be the QQ gateway `msg_id` of the inbound message being
    /// replied to — never a fabricated, stale, or cross-conversation value.
    /// Empty strings are dropped to avoid sending `msg_id=""` which the QQ
    /// API rejects with a different validation code.
    ///
    /// Per the task scope (#7872), `msg_id` is only attached for group
    /// recipients (`group:<openid>`); private (c2c) sends use the same body
    /// builder but the field is intentionally omitted unless and until
    /// we have evidence the c2c endpoint requires it.
    fn build_send_body(
        recipient: &str,
        mut payload: serde_json::Value,
        in_reply_to: Option<&str>,
    ) -> serde_json::Value {
        if let Some(reply_id) = in_reply_to {
            if !reply_id.is_empty() && recipient.starts_with("group:") {
                let obj = payload
                    .as_object_mut()
                    .expect("QQ send body must be a JSON object");
                // Preserve a caller-supplied msg_id; only insert the
                // passive-reply id when none is present. This avoids
                // accidentally clobbering an id a higher layer chose
                // intentionally and keeps the JSON shape with exactly one
                // `msg_id` key.
                obj.entry("msg_id".to_string())
                    .or_insert_with(|| serde_json::Value::String(reply_id.to_string()));
            }
        }
        payload
    }

    /// Send a media message (msg_type=7) with an already-uploaded file_info.
    async fn send_media_message(
        &self,
        recipient: &str,
        file_info: &str,
        in_reply_to: Option<&str>,
    ) -> anyhow::Result<()> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);

        let url = format!("{QQ_API_BASE}/v2/{scope}/{id}/messages");
        ensure_https(&url)?;

        let body = Self::build_send_body(
            recipient,
            json!({
                "msg_type": 7,
                "media": {
                    "file_info": file_info,
                },
                "msg_seq": next_msg_seq(),
            }),
            in_reply_to,
        );

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
        in_reply_to: Option<&str>,
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
            self.send_media_message(recipient, &file_info, in_reply_to)
                .await?;
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
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"target": target})),
                    "using cached upload for"
                );
                self.send_media_message(recipient, &cached_file_info, in_reply_to)
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

            self.send_media_message(recipient, &file_info, in_reply_to)
                .await?;
        }

        Ok(())
    }

    async fn compose_qq_message(&self, payload: &serde_json::Value) -> Option<QQComposedMessage> {
        let text = payload
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();

        let mut markers: Vec<String> = Vec::new();
        let mut voice_transcripts: Vec<String> = Vec::new();
        let mut voice_dedup_parts: Vec<QQVoiceDedupPart> = Vec::new();

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
                let is_voice = Self::is_voice_attachment(content_type, filename);
                let (download_url, save_filename) = if is_voice {
                    if let Some(wav_url) = att
                        .get("voice_wav_url")
                        .and_then(|u| u.as_str())
                        .filter(|u| !u.trim().is_empty())
                    {
                        let fixed = fix_qq_url(wav_url);
                        let wav_name = voice_wav_filename(&fixed);
                        (fixed, wav_name)
                    } else {
                        (url.clone(), filename.to_string())
                    }
                } else {
                    (url.clone(), filename.to_string())
                };

                // Try to download to workspace
                let (location, local_audio_data) = if let Some(ref ws) = self.workspace_dir {
                    let dir = ws.join("qq_files");
                    match self
                        .download_attachment(&download_url, &dir, &save_filename)
                        .await
                    {
                        Ok((local_path, bytes)) => (
                            local_path.display().to_string(),
                            if is_voice { Some(bytes) } else { None },
                        ),
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "failed to download attachment"
                            );
                            (url.clone(), None)
                        }
                    }
                } else {
                    (url.clone(), None)
                };

                if is_voice {
                    // For voice: include ASR transcription text (aligned with
                    // openclaw-qqbot format: "[语音消息] transcribed text")
                    // Also keep the file path marker for future multimodal support
                    markers.push(format!("[{marker_type}:{location}]"));
                    let voice_transcript = if let Some(asr_text) = att
                        .get("asr_refer_text")
                        .and_then(|t| t.as_str())
                        .and_then(Self::normalize_voice_transcript)
                    {
                        Some(asr_text)
                    } else if let Some(audio_data) = local_audio_data.as_deref()
                        && let Some(transcript) = self
                            .try_transcribe_audio_data(audio_data, &save_filename)
                            .await
                    {
                        Self::normalize_voice_transcript(&transcript)
                    } else {
                        None
                    };

                    if let Some(transcript) = voice_transcript {
                        voice_transcripts.push(transcript.clone());
                        if let Some(source_id) = Self::voice_attachment_source_id(att) {
                            voice_dedup_parts.push(QQVoiceDedupPart {
                                source_id,
                                transcript,
                            });
                        }
                    };
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

        Some(QQComposedMessage {
            content: parts.join("\n"),
            voice_dedup_parts,
        })
    }

    /// Download an attachment to the local workspace directory.
    async fn download_attachment(
        &self,
        url: &str,
        dir: &Path,
        filename: &str,
    ) -> anyhow::Result<(PathBuf, Vec<u8>)> {
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

        let bytes = resp.bytes().await?.to_vec();
        tokio::fs::write(&dest, &bytes).await?;

        Ok((dest, bytes))
    }

    async fn try_transcribe_audio_data(&self, audio_data: &[u8], filename: &str) -> Option<String> {
        let manager = self.transcription_manager.as_deref()?;

        if audio_data.len() as u64 > QQ_MAX_AUDIO_TRANSCRIPTION_BYTES {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "bytes": audio_data.len(),
                        "max_bytes": QQ_MAX_AUDIO_TRANSCRIPTION_BYTES,
                    })),
                "QQ audio attachment exceeds transcription size limit"
            );
            return None;
        }

        match manager.transcribe(audio_data, filename).await {
            Ok(text) => {
                let text = text.trim();
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "QQ audio transcription failed"
                );
                None
            }
        }
    }

    /// Send a markdown text message (msg_type=2).
    async fn send_text_markdown(
        &self,
        recipient: &str,
        content: &str,
        in_reply_to: Option<&str>,
    ) -> anyhow::Result<()> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);

        let url = format!("{QQ_API_BASE}/v2/{scope}/{id}/messages");
        ensure_https(&url)?;

        let body = Self::build_send_body(
            recipient,
            json!({
                "markdown": {
                    "content": content,
                },
                "msg_type": 2,
                "msg_seq": next_msg_seq(),
            }),
            in_reply_to,
        );

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

impl ::zeroclaw_api::attribution::Attributable for QQChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Qq)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for QQChannel {
    fn name(&self) -> &str {
        "qq"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let (cleaned_text, attachments) = parse_qq_attachment_markers(&message.content);
        let in_reply_to: Option<&str> = message
            .in_reply_to
            .as_deref()
            .filter(|id| !id.is_empty());

        if attachments.is_empty() {
            // No media markers — send as markdown (original path)
            return self
                .send_text_markdown(&message.recipient, &message.content, in_reply_to)
                .await;
        }

        // Send cleaned text first (if non-empty)
        if !cleaned_text.is_empty() {
            self.send_text_markdown(&message.recipient, &cleaned_text, in_reply_to)
                .await?;
        }

        // Send each media attachment
        for attachment in &attachments {
            if let Err(e) = self.send_attachment(&message.recipient, attachment, in_reply_to).await {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"target": attachment.target, "error": format!("{}", e)})), "failed to send media attachment; falling back to text");
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
                self.send_text_markdown(&message.recipient, &fallback, in_reply_to)
                    .await?;
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "authenticating..."
        );
        let token = self.get_token().await?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "fetching gateway URL..."
        );
        let gw_url = self.get_gateway_url(&token).await?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "connecting to gateway WebSocket..."
        );
        let (ws_stream, _) = zeroclaw_config::schema::ws_connect_with_proxy(
            &gw_url,
            "channel.qq",
            self.proxy_url.as_deref(),
        )
        .await?;
        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read.next().await.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"phase": "gateway_hello"})),
                "qq: gateway closed before Hello frame"
            );
            anyhow::Error::msg("no hello frame")
        })??;
        let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())?;
        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        // Check if we can resume a previous session
        let stored_session = self.session_id.read().await.clone();
        let stored_seq = *self.last_sequence.read().await;

        if let (Some(sid), Some(seq)) = (&stored_session, stored_seq) {
            // Attempt Resume (opcode 6)
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sid": sid, "seq": seq})),
                "attempting session resume (session_id=, seq=)"
            );
            let resume = json!({
                "op": 6,
                "d": {
                    "token": format!("QQBot {token}"),
                    "session_id": sid,
                    "seq": seq,
                }
            });
            write.send(Message::Text(resume.to_string().into())).await?;
        } else {
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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "connected and sent Identify"
            );
        }

        let mut sequence: i64 = stored_seq.unwrap_or(-1);

        // Track consecutive missed heartbeat ACKs.  The previous logic
        // killed the connection on the *first* missed ACK which is overly
        // aggressive -- transient network hiccups or brief server-side GC
        // pauses can cause a single ACK to be delayed.  We now allow up to
        // `MAX_MISSED_ACKS` consecutive misses before declaring the
        // connection dead.
        const MAX_MISSED_ACKS: u32 = 3;
        let mut missed_ack_count: u32 = 0;

        // Spawn heartbeat timer.
        //
        // We add a small grace period (10% of the server-provided interval,
        // capped at 5s) so that a slightly-delayed ACK does not immediately
        // count as missed.
        let hb_interval = heartbeat_interval;
        let grace_ms: u64 = (hb_interval / 10).min(5_000);
        let effective_interval = hb_interval.saturating_add(grace_ms);

        let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
        zeroclaw_spawn::spawn!(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(effective_interval));
            loop {
                interval.tick().await;
                if hb_tx.send(()).await.is_err() {
                    break;
                }
            }
        });

        // Reason the loop exited — used to decide error type
        enum ExitReason {
            Reconnect,
            InvalidSession,
            Close(Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>),
            StreamEnded,
            HeartbeatTimeout,
            WriteFailed,
            ChannelClosed,
        }

        let exit_reason;

        'outer: loop {
            tokio::select! {
                _ = hb_rx.recv() => {
                    // Increment the missed-ACK counter.  Only declare the
                    // connection dead after MAX_MISSED_ACKS consecutive
                    // heartbeats go un-acknowledged.
                    if missed_ack_count > 0 {
                        if missed_ack_count >= MAX_MISSED_ACKS {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"missed_ack_count": missed_ack_count, "hb_interval": hb_interval, "grace_ms": grace_ms})), "consecutive heartbeat ACKs missed (interval ms + ms grace); connection appears zombied");
                            exit_reason = ExitReason::HeartbeatTimeout;
                            break;
                        }
                        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"missed_ack_count": missed_ack_count, "MAX_MISSED_ACKS": MAX_MISSED_ACKS})), "heartbeat ACK missed (/); tolerating transient delay");
                    }
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write
                        .send(Message::Text(hb.to_string().into()))
                        .await
                        .is_err()
                    {
                        exit_reason = ExitReason::WriteFailed;
                        break;
                    }
                    missed_ack_count += 1;
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Ping(payload))) => {
                            if write.send(Message::Pong(payload)).await.is_err() {
                                exit_reason = ExitReason::WriteFailed;
                                break;
                            }
                            continue;
                        }
                        Some(Ok(Message::Close(frame))) => {
                            exit_reason = ExitReason::Close(frame);
                            break;
                        }
                        None => {
                            exit_reason = ExitReason::StreamEnded;
                            break;
                        }
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Track sequence number
                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                        self.record_gateway_sequence(s).await;
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
                                exit_reason = ExitReason::WriteFailed;
                                break;
                            }
                            missed_ack_count += 1;
                            continue;
                        }
                        // Reconnect
                        7 => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "received Reconnect (op 7); will resume");
                            exit_reason = ExitReason::Reconnect;
                            break;
                        }
                        // Invalid Session
                        9 => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "received Invalid Session (op 9); clearing session for fresh auth");
                            exit_reason = ExitReason::InvalidSession;
                            break;
                        }
                        // Heartbeat ACK
                        11 => {
                            missed_ack_count = 0;
                            continue;
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

                    // Capture session_id from READY event for future resume
                    if event_type == "READY" || event_type == "RESUMED" {
                        if let Some(sid) = d.get("session_id").and_then(|s| s.as_str()) {
                            *self.session_id.write().await = Some(sid.to_string());
                            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"sid": sid, "event_type": event_type})), "session established (session_id=, event=)");
                        }
                        continue;
                    }

                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"event_type": event_type, "d": d})), "event_type= payload=");

                    match event_type {
                        "C2C_MESSAGE_CREATE" => {
                            let msg_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            if self.is_duplicate(msg_id).await {
                                continue;
                            }

                            let author_id = d.get("author").and_then(|a| a.get("id")).and_then(|i| i.as_str()).unwrap_or("unknown");
                            // For QQ, user_openid is the identifier
                            let user_openid = d.get("author").and_then(|a| a.get("user_openid")).and_then(|u| u.as_str()).unwrap_or(author_id);

                            if !self.is_user_allowed(user_openid) {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_openid": user_openid})), "ignoring C2C message from unauthorized user");
                                continue;
                            }

                            let chat_id = format!("user:{user_openid}");
                            let platform_voice_dedup_parts = Self::platform_voice_dedup_parts(d);
                            if self
                                .is_duplicate_voice_redelivery(
                                    event_type,
                                    &chat_id,
                                    user_openid,
                                    &platform_voice_dedup_parts,
                                )
                                .await
                            {
                                continue;
                            }

                            let Some(composed) = self.compose_qq_message(d).await else {
                                continue;
                            };
                            let fallback_voice_dedup_parts: Vec<QQVoiceDedupPart> = composed
                                .voice_dedup_parts
                                .iter()
                                .filter(|part| !platform_voice_dedup_parts.contains(part))
                                .cloned()
                                .collect();
                            if self
                                .is_duplicate_voice_redelivery(
                                    event_type,
                                    &chat_id,
                                    user_openid,
                                    &fallback_voice_dedup_parts,
                                )
                                .await
                            {
                                continue;
                            }

                            let channel_msg = ChannelMessage {
                                // Use the QQ gateway msg_id as the canonical
                                // ChannelMessage id so passive-reply sends can
                                // attach it as `msg_id` and avoid the
                                // active-message permission failure (code
                                // 40034102). See #7872.
                                id: msg_id.to_string(),
                                sender: user_openid.to_string(),
                                reply_target: chat_id,
                                content: composed.content,
                                channel: "qq".to_string(),
                channel_alias: Some(self.alias.clone()),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                thread_ts: None,
                                interruption_scope_id: None,
                    attachments: vec![],
                                subject: None,
                            };

                            if tx.send(channel_msg).await.is_err() {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "message channel closed");
                                exit_reason = ExitReason::ChannelClosed;
                                break 'outer;
                            }
                        }
                        "GROUP_AT_MESSAGE_CREATE" => {
                            let msg_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            if self.is_duplicate(msg_id).await {
                                continue;
                            }

                            let author_id = d.get("author").and_then(|a| a.get("member_openid")).and_then(|m| m.as_str()).unwrap_or("unknown");

                            if !self.is_user_allowed(author_id) {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"author_id": author_id})), "ignoring group message from unauthorized user");
                                continue;
                            }

                            let group_openid = d.get("group_openid").and_then(|g| g.as_str()).unwrap_or("unknown");
                            let chat_id = format!("group:{group_openid}");
                            let platform_voice_dedup_parts = Self::platform_voice_dedup_parts(d);
                            if self
                                .is_duplicate_voice_redelivery(
                                    event_type,
                                    &chat_id,
                                    author_id,
                                    &platform_voice_dedup_parts,
                                )
                                .await
                            {
                                continue;
                            }

                            let Some(composed) = self.compose_qq_message(d).await else {
                                continue;
                            };
                            let fallback_voice_dedup_parts: Vec<QQVoiceDedupPart> = composed
                                .voice_dedup_parts
                                .iter()
                                .filter(|part| !platform_voice_dedup_parts.contains(part))
                                .cloned()
                                .collect();
                            if self
                                .is_duplicate_voice_redelivery(
                                    event_type,
                                    &chat_id,
                                    author_id,
                                    &fallback_voice_dedup_parts,
                                )
                                .await
                            {
                                continue;
                            }

                            let channel_msg = ChannelMessage {
                                // Use the QQ gateway msg_id as the canonical
                                // ChannelMessage id so passive-reply sends can
                                // attach it as `msg_id` to the group send
                                // body, classifying it as a passive reply
                                // rather than an active message (which would
                                // require an additional Tencent permission
                                // grant — QQ API error code 40034102).
                                // See #7872.
                                id: msg_id.to_string(),
                                sender: author_id.to_string(),
                                reply_target: chat_id,
                                content: composed.content,
                                channel: "qq".to_string(),
                channel_alias: Some(self.alias.clone()),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                thread_ts: None,
                                interruption_scope_id: None,
                    attachments: vec![],
                                subject: None,
                            };

                            if tx.send(channel_msg).await.is_err() {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "message channel closed");
                                exit_reason = ExitReason::ChannelClosed;
                                break 'outer;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Persist sequence number for potential resume on next reconnect
        *self.last_sequence.write().await = if sequence >= 0 { Some(sequence) } else { None };

        match exit_reason {
            ExitReason::InvalidSession => {
                // Clear stored session so next reconnect does a fresh Identify
                *self.session_id.write().await = None;
                *self.last_sequence.write().await = None;
                anyhow::bail!(
                    "QQ WebSocket connection closed: invalid session (fresh auth required)"
                )
            }
            ExitReason::Reconnect => {
                // Session state preserved — supervisor will reconnect and we'll attempt Resume
                anyhow::bail!(
                    "QQ WebSocket connection closed: server requested reconnect (resume will be attempted)"
                )
            }
            ExitReason::Close(ref frame) => {
                let (code, reason) = frame
                    .as_ref()
                    .map(|f| (f.code.to_string(), f.reason.to_string()))
                    .unwrap_or_else(|| ("unknown".into(), "none".into()));
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"code": code.to_string(), "reason": reason.to_string()})), "WebSocket closed with code=, reason=\"\"; resume will be attempted on reconnect");
                anyhow::bail!(
                    "QQ WebSocket connection closed: close_code={code}, reason=\"{reason}\""
                )
            }
            ExitReason::StreamEnded => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "WebSocket stream ended unexpectedly; resume will be attempted on reconnect"
                );
                anyhow::bail!("QQ WebSocket connection closed: stream ended unexpectedly")
            }
            ExitReason::HeartbeatTimeout => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"MAX_MISSED_ACKS": MAX_MISSED_ACKS})),
                    "heartbeat timeout after consecutive missed ACKs; resume will be attempted on reconnect"
                );
                anyhow::bail!(
                    "QQ WebSocket connection closed: heartbeat ACK timeout \
                     ({MAX_MISSED_ACKS} consecutive missed ACKs)"
                )
            }
            ExitReason::WriteFailed => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "WebSocket write failed; resume will be attempted on reconnect"
                );
                anyhow::bail!("QQ WebSocket connection closed: write failed")
            }
            ExitReason::ChannelClosed => {
                anyhow::bail!("QQ WebSocket connection closed: internal message channel closed")
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.fetch_access_token_with_retry().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_name() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        assert_eq!(ch.name(), "qq");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(|| vec!["user123".into()]),
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[tokio::test]
    async fn test_dedup() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        assert!(!ch.is_duplicate("msg1").await);
        assert!(ch.is_duplicate("msg1").await);
        assert!(!ch.is_duplicate("msg2").await);
    }

    #[tokio::test]
    async fn test_dedup_empty_id() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        assert!(!ch.is_duplicate("").await);
        assert!(!ch.is_duplicate("").await);
    }

    #[tokio::test]
    async fn gateway_sequence_is_persisted_as_dispatch_frames_arrive() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );

        ch.record_gateway_sequence(42).await;

        assert_eq!(*ch.last_sequence.read().await, Some(42));
    }

    #[tokio::test]
    async fn voice_redelivery_with_new_msg_id_is_deduped_by_transcript() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );

        let payload = json!({
            "attachments": [{
                "content_type": "voice",
                "filename": "voice.amr",
                "url": "https://cdn.example.com/voice.amr?sig=first",
                "voice_wav_url": "https://cdn.example.com/voice.wav?sig=first",
                "asr_refer_text": "哈喽哈喽，你在干嘛？"
            }]
        });
        let redelivered = json!({
            "attachments": [{
                "content_type": "voice",
                "filename": "voice.amr",
                "url": "https://cdn.example.com/voice.amr?sig=second",
                "voice_wav_url": "https://cdn.example.com/voice.wav?sig=second",
                "asr_refer_text": " 哈喽哈喽，你在干嘛？ "
            }]
        });
        let ordinary_text = json!({
            "content": "<VOICE_TRANSCRIPTION>哈喽哈喽，你在干嘛？</VOICE_TRANSCRIPTION>"
        });

        assert!(
            !ch.is_duplicate_voice_redelivery(
                "C2C_MESSAGE_CREATE",
                "user:qq_user",
                "qq_user",
                &QQChannel::platform_voice_dedup_parts(&payload),
            )
            .await
        );
        assert!(
            ch.is_duplicate_voice_redelivery(
                "C2C_MESSAGE_CREATE",
                "user:qq_user",
                "qq_user",
                &QQChannel::platform_voice_dedup_parts(&redelivered),
            )
            .await
        );
        assert!(
            !ch.is_duplicate_voice_redelivery(
                "C2C_MESSAGE_CREATE",
                "user:qq_user",
                "qq_user",
                &QQChannel::platform_voice_dedup_parts(&ordinary_text),
            )
            .await
        );
    }

    #[tokio::test]
    async fn voice_redelivery_dedup_keeps_distinct_audio_sources() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );

        let first = vec![QQVoiceDedupPart {
            source_id: "voice:voice.amr:https://cdn.example.com/first.wav".into(),
            transcript: "yes".into(),
        }];
        let second = vec![QQVoiceDedupPart {
            source_id: "voice:voice.amr:https://cdn.example.com/second.wav".into(),
            transcript: "yes".into(),
        }];

        assert!(
            !ch.is_duplicate_voice_redelivery(
                "C2C_MESSAGE_CREATE",
                "user:qq_user",
                "qq_user",
                &first,
            )
            .await
        );
        assert!(
            !ch.is_duplicate_voice_redelivery(
                "C2C_MESSAGE_CREATE",
                "user:qq_user",
                "qq_user",
                &second,
            )
            .await
        );
    }

    #[test]
    fn v2_allowed_users_fold_into_peer_groups() {
        // V2 `[channels.qq].allowed_users` migrates into a synthesized
        // `[peer_groups.qq_default]` block in V3, while the channel block
        // itself survives under the bridge alias `default`.
        let v2_toml = r#"
schema_version = 2

[channels.qq]
enabled = true
app_id = "12345"
app_secret = "secret_abc"
allowed_users = ["user1"]
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 qq config migrates to V3");
        let qq = cfg
            .channels
            .qq
            .get("default")
            .expect("V2 qq folds under alias `default`");
        assert_eq!(qq.app_id, "12345");
        assert_eq!(qq.app_secret, "secret_abc");

        let group = cfg
            .peer_groups
            .get("qq_default")
            .expect("qq allow-list synthesizes [peer_groups.qq_default]");
        assert_eq!(group.channel, "qq");
        let peers: Vec<&str> = group.external_peers.iter().map(|p| p.as_str()).collect();
        assert_eq!(peers, vec!["user1"]);
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

    fn local_whisper_transcription_config(
        url: String,
    ) -> zeroclaw_config::schema::TranscriptionConfig {
        zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: None,
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "whisper-large-v3".to_string(),
            language: None,
            initial_prompt: None,
            max_audio_bytes: None,
            max_duration_secs: 600,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url,
                bearer_token: Some("test_token".to_string()),
                max_audio_bytes: 25_000_000,
                timeout_secs: 300,
            }),
            transcribe_non_ptt_audio: false,
        }
    }

    async fn composed_content(ch: &QQChannel, payload: &serde_json::Value) -> Option<String> {
        ch.compose_qq_message(payload)
            .await
            .map(|message| message.content)
    }

    #[tokio::test]
    async fn test_compose_message_content_text_only() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let payload = json!({ "content": "  hello world  " });
        assert_eq!(
            composed_content(&ch, &payload).await,
            Some("hello world".to_string())
        );
    }

    #[tokio::test]
    async fn test_compose_message_content_image_attachment() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let payload = json!({
            "content": "   ",
            "attachments": [{
                "content_type": "image/jpg",
                "url": "https://cdn.example.com/a.jpg"
            }]
        });
        assert_eq!(
            composed_content(&ch, &payload).await,
            Some("[IMAGE:https://cdn.example.com/a.jpg]".to_string())
        );
    }

    #[tokio::test]
    async fn test_compose_message_content_text_and_attachments() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let payload = json!({
            "content": "Here is an image",
            "attachments": [
                { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
                { "filename": "b.jpeg", "url": "https://cdn.example.com/b.jpeg" }
            ]
        });
        assert_eq!(
            composed_content(&ch, &payload).await,
            Some(
                "Here is an image\n[IMAGE:https://cdn.example.com/a.png]\n[IMAGE:https://cdn.example.com/b.jpeg]"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn test_compose_all_attachment_types() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let payload = json!({
            "content": "",
            "attachments": [
                { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
                { "content_type": "audio/mpeg", "url": "https://cdn.example.com/b.mp3" },
                { "content_type": "video/mp4", "url": "https://cdn.example.com/c.mp4" },
                { "content_type": "application/pdf", "url": "https://cdn.example.com/d.pdf" }
            ]
        });
        let result = composed_content(&ch, &payload).await.unwrap();
        assert!(result.contains("[IMAGE:"));
        assert!(result.contains("[VOICE:"));
        assert!(result.contains("[VIDEO:"));
        assert!(result.contains("[DOCUMENT:"));
    }

    #[tokio::test]
    async fn test_compose_voice_attachment_transcribes_when_asr_missing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"audio bytes"))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"text": " configured transcript "})),
            )
            .mount(&mock_server)
            .await;

        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        )
        .with_workspace_dir(workspace.path().to_path_buf())
        .with_transcription(local_whisper_transcription_config(format!(
            "{}/v1/audio/transcriptions",
            mock_server.uri()
        )));

        let payload = json!({
            "content": "",
            "attachments": [{
                "content_type": "voice",
                "filename": "voice.amr",
                "url": "https://cdn.example.com/voice.amr",
                "voice_wav_url": format!("{}/download?sig=abc", mock_server.uri())
            }]
        });

        let result = composed_content(&ch, &payload).await.unwrap();
        assert!(
            result.contains("<VOICE_TRANSCRIPTION>configured transcript</VOICE_TRANSCRIPTION>")
        );
        assert!(result.contains("[VOICE:"));
        assert!(result.contains("qq_files/download_"));
        assert!(result.contains(".wav]"));
    }

    #[tokio::test]
    async fn test_compose_voice_attachment_preserves_platform_asr() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/voice.wav"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"audio bytes"))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "fallback"})))
            .expect(0)
            .mount(&mock_server)
            .await;

        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        )
        .with_workspace_dir(workspace.path().to_path_buf())
        .with_transcription(local_whisper_transcription_config(format!(
            "{}/v1/audio/transcriptions",
            mock_server.uri()
        )));

        let payload = json!({
            "content": "",
            "attachments": [{
                "content_type": "voice",
                "filename": "voice.amr",
                "url": "https://cdn.example.com/voice.amr",
                "voice_wav_url": format!("{}/voice.wav", mock_server.uri()),
                "asr_refer_text": "platform transcript"
            }]
        });

        let result = composed_content(&ch, &payload).await.unwrap();
        assert!(result.contains("<VOICE_TRANSCRIPTION>platform transcript</VOICE_TRANSCRIPTION>"));
        assert!(!result.contains("fallback"));
    }

    #[tokio::test]
    async fn test_compose_fixes_double_slash_url() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let payload = json!({
            "content": "",
            "attachments": [{
                "content_type": "image/png",
                "url": "//cdn.example.com/a.png"
            }]
        });
        let result = composed_content(&ch, &payload).await.unwrap();
        assert!(result.contains("https://cdn.example.com/a.png"));
        // Ensure the raw `//` prefix was replaced with `https:`
        assert!(!result.starts_with("[IMAGE://"));
    }

    #[tokio::test]
    async fn test_compose_fallback_no_workspace() {
        // Without workspace_dir, attachments use URLs directly
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let payload = json!({
            "content": "text",
            "attachments": [{
                "content_type": "application/pdf",
                "filename": "report.pdf",
                "url": "https://cdn.example.com/report.pdf"
            }]
        });
        let result = composed_content(&ch, &payload).await.unwrap();
        assert!(result.contains("[DOCUMENT:https://cdn.example.com/report.pdf]"));
    }

    #[tokio::test]
    async fn test_compose_drops_empty_url() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let payload = json!({
            "content": "   ",
            "attachments": [{
                "content_type": "image/png",
                "url": "   "
            }]
        });
        assert_eq!(composed_content(&ch, &payload).await, None);
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

    // --- build_send_body passive-reply msg_id propagation (#7872) ---
    //
    // QQ Bot v2 group sends must carry the triggering inbound `msg_id` so the
    // server classifies the send as a passive reply. Without it, the API
    // rejects with `40034102` unless the bot holds active-message
    // permissions. See zeroclaw-labs/zeroclaw#7872.

    fn minimal_group_body() -> serde_json::Value {
        json!({
            "markdown": { "content": "hi" },
            "msg_type": 2,
            "msg_seq": 1,
        })
    }

    fn minimal_c2c_body() -> serde_json::Value {
        json!({
            "markdown": { "content": "hi" },
            "msg_type": 2,
            "msg_seq": 1,
        })
    }

    #[test]
    fn test_build_send_body_group_attaches_msg_id_for_passive_reply() {
        let body = QQChannel::build_send_body(
            "group:OPENID_G1",
            minimal_group_body(),
            Some("INBOUND_MSG_ID_42"),
        );
        assert_eq!(
            body.get("msg_id").and_then(|v| v.as_str()),
            Some("INBOUND_MSG_ID_42"),
            "group sends must include the triggering msg_id (#7872)"
        );
        // Original fields preserved.
        assert_eq!(body["msg_type"], 2);
        assert_eq!(body["markdown"]["content"], "hi");
    }

    #[test]
    fn test_build_send_body_group_without_in_reply_to_omits_msg_id() {
        let body = QQChannel::build_send_body("group:OPENID_G1", minimal_group_body(), None);
        assert!(
            body.get("msg_id").is_none(),
            "no passive-reply id => msg_id must be absent (do not fabricate)"
        );
    }

    #[test]
    fn test_build_send_body_group_with_empty_in_reply_to_omits_msg_id() {
        // Empty string is treated as "no id" — sending msg_id="" would be a
        // different QQ API validation failure.
        let body = QQChannel::build_send_body("group:OPENID_G1", minimal_group_body(), Some(""));
        assert!(
            body.get("msg_id").is_none(),
            "empty msg_id must not be forwarded to the QQ API"
        );
    }

    #[test]
    fn test_build_send_body_private_dm_omits_msg_id() {
        // Scope per #7872 is intentionally narrow: only group recipients.
        // Private (user:/c2c) sends must not attach msg_id — that would
        // either be rejected by the QQ API as an unknown field or silently
        // mark the send as a passive reply to a non-existent thread.
        let body = QQChannel::build_send_body(
            "user:OPENID_U1",
            minimal_c2c_body(),
            Some("INBOUND_MSG_ID_42"),
        );
        assert!(
            body.get("msg_id").is_none(),
            "user-scope sends must not carry msg_id per #7872 scope"
        );
    }

    #[test]
    fn test_build_send_body_msg_id_is_passed_through_verbatim() {
        // Regression guard against accidental id mangling / re-encoding.
        let raw = "ROBOT_INBOUND.123.abcdef";
        let body = QQChannel::build_send_body("group:G", minimal_group_body(), Some(raw));
        assert_eq!(body.get("msg_id").and_then(|v| v.as_str()), Some(raw));
    }

    #[test]
    fn test_build_send_body_does_not_duplicate_msg_id_when_already_present() {
        // The helper should not stomp on a caller-provided msg_id; it just
        // inserts when an in_reply_to is supplied. Verify we never silently
        // overwrite or duplicate the key.
        let mut payload = minimal_group_body();
        payload.as_object_mut().unwrap().insert(
            "msg_id".to_string(),
            serde_json::Value::String("preexisting".to_string()),
        );
        let body = QQChannel::build_send_body(
            "group:G",
            payload,
            Some("INBOUND_MSG_ID_42"),
        );
        let count = body
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k == "msg_id")
            .count();
        assert_eq!(count, 1, "msg_id key must not be duplicated");
        assert_eq!(
            body.get("msg_id").and_then(|v| v.as_str()),
            Some("preexisting"),
            "pre-existing msg_id must be preserved over a later in_reply_to"
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
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
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
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        let key = QQChannel::upload_cache_key(b"test_data", "group", "g1", QQMediaFileType::Video);

        // Set with 0 TTL (already expired considering 60s safety margin)
        ch.set_cached_upload(key.clone(), "old_info".into(), 0)
            .await;

        // Should miss due to expiry
        assert!(ch.get_cached_upload(&key).await.is_none());
    }

    // --- Auth retry tests ---

    #[test]
    fn test_auth_retry_constants_are_sensible() {
        const {
            assert!(AUTH_RETRY_MAX_ATTEMPTS >= 2, "should retry at least once");
            assert!(
                AUTH_RETRY_INITIAL_BACKOFF_MS > 0,
                "initial backoff must be positive"
            );
            assert!(
                AUTH_RETRY_MAX_BACKOFF_MS >= AUTH_RETRY_INITIAL_BACKOFF_MS,
                "max backoff must be >= initial"
            );
        }
    }

    #[test]
    fn test_auth_retry_backoff_stays_within_bounds() {
        // Simulate the backoff progression and verify it caps at max
        let mut backoff = AUTH_RETRY_INITIAL_BACKOFF_MS;
        for _ in 1..AUTH_RETRY_MAX_ATTEMPTS {
            backoff = (backoff * 2).min(AUTH_RETRY_MAX_BACKOFF_MS);
        }
        assert!(
            backoff <= AUTH_RETRY_MAX_BACKOFF_MS,
            "backoff must never exceed the configured maximum"
        );
    }

    #[tokio::test]
    async fn test_get_token_returns_cached_token_without_fetch() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        // Pre-populate the token cache with a token that expires far in the future
        let future_expiry = now_secs() + 3600;
        *ch.token_cache.write().await = Some(("cached_tok".to_string(), future_expiry));

        // get_token should return the cached value without hitting the network
        let tok = ch.get_token().await.unwrap();
        assert_eq!(tok, "cached_tok");
    }

    #[tokio::test]
    async fn test_get_token_refreshes_expired_cache() {
        let ch = QQChannel::new(
            "id".into(),
            "secret".into(),
            "qq_test_alias",
            Arc::new(Vec::new),
        );
        // Pre-populate with an already-expired token
        *ch.token_cache.write().await = Some(("old_tok".to_string(), 0));

        // get_token should try to refresh -- will fail because there's no real
        // server, but the important thing is it doesn't return the stale token.
        let result = ch.get_token().await;
        assert!(
            result.is_err(),
            "should fail when token expired and no server available"
        );
    }

    // --- Heartbeat stability tests ---

    #[test]
    fn test_heartbeat_grace_period_calculation() {
        // The grace period is 10% of the server interval, capped at 5000ms.
        let cases: Vec<(u64, u64)> = vec![
            (41_250, 4_125),  // default QQ interval
            (30_000, 3_000),  // smaller interval
            (60_000, 5_000),  // larger interval, capped at 5s
            (100_000, 5_000), // very large, still capped
            (5_000, 500),     // small interval
            (0, 0),           // degenerate zero
        ];
        for (interval, expected_grace) in cases {
            let grace: u64 = (interval / 10).min(5_000);
            assert_eq!(
                grace, expected_grace,
                "grace for interval {interval} should be {expected_grace}"
            );
            let effective = interval.saturating_add(grace);
            assert!(effective >= interval);
        }
    }

    #[test]
    fn test_missed_ack_counter_logic() {
        let max_missed: u32 = 3;
        let mut missed: u32 = 0;

        // First tick: counter is 0, send heartbeat
        assert!(missed < max_missed);
        missed += 1;
        assert_eq!(missed, 1, "counter should be 1 after first heartbeat");

        // ACK received: reset
        missed = 0;
        assert_eq!(missed, 0, "counter should reset on ACK");

        // 3 consecutive misses without ACK
        for _ in 0..max_missed {
            assert!(
                missed < max_missed,
                "should not reach zombie state before {max_missed} misses"
            );
            missed += 1;
        }
        assert!(
            missed >= max_missed,
            "should declare zombie after {max_missed} missed ACKs"
        );
    }

    #[test]
    fn test_missed_ack_counter_reset_on_ack() {
        let max_missed: u32 = 3;
        let mut missed: u32 = 0;

        missed += 1;
        missed += 1;
        assert_eq!(missed, 2);

        // ACK arrives: reset
        missed = 0;
        assert_eq!(missed, 0);

        // One more miss, still under threshold
        missed += 1;
        assert!(missed < max_missed);
    }

    #[test]
    fn test_effective_interval_never_overflows() {
        let interval = u64::MAX;
        let grace: u64 = (interval / 10).min(5_000);
        let effective = interval.saturating_add(grace);
        assert_eq!(effective, u64::MAX);
    }
}
