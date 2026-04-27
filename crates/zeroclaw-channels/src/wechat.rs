//! WeChat personal iLink Bot channel.
//!
//! Note: the iLink consent screen ("Connect X to Weixin") shows the bot name
//! from the iLink developer portal, not from ZeroClaw config. Users who
//! register their own iLink bot will see their own name there.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_runtime::security::pairing::PairingGuard;

const DEFAULT_API_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

/// Long-poll timeout for getUpdates (server may hold the request up to this).
const LONG_POLL_TIMEOUT_MS: u64 = 35_000;
/// Regular API request timeout.
const API_TIMEOUT: Duration = Duration::from_secs(15);

/// Session-expired error code returned by the iLink API.
const SESSION_EXPIRED_ERRCODE: i64 = -14;
/// Pause duration after session expiry before retrying.
const SESSION_PAUSE_DURATION: Duration = Duration::from_secs(60 * 60);
/// Maximum consecutive API failures before backing off.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// Back-off delay after reaching max consecutive failures.
const BACKOFF_DELAY: Duration = Duration::from_secs(30);
/// Retry delay for a single failure.
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// QR code long-poll timeout.
const QR_POLL_TIMEOUT: Duration = Duration::from_secs(35);
/// Maximum QR code refresh attempts.
const MAX_QR_REFRESH: u32 = 3;
/// Total QR scan wait timeout.
const QR_SCAN_TIMEOUT: Duration = Duration::from_secs(480);

const WECHAT_BIND_COMMAND: &str = "/bind";

/// iLink Bot message types.
const MESSAGE_TYPE_BOT: u32 = 2;
/// iLink Bot message state.
const MESSAGE_STATE_FINISH: u32 = 2;
/// iLink Bot message item type: text.
const ITEM_TYPE_TEXT: u32 = 1;
/// iLink Bot message item type: image.
const ITEM_TYPE_IMAGE: u32 = 2;
/// iLink Bot message item type: voice.
const ITEM_TYPE_VOICE: u32 = 3;
/// iLink Bot message item type: file.
const ITEM_TYPE_FILE: u32 = 4;
/// iLink Bot message item type: video.
const ITEM_TYPE_VIDEO: u32 = 5;

/// getUploadUrl media type: image.
const UPLOAD_MEDIA_TYPE_IMAGE: u32 = 1;
/// getUploadUrl media type: video.
const UPLOAD_MEDIA_TYPE_VIDEO: u32 = 2;
/// getUploadUrl media type: file/document.
const UPLOAD_MEDIA_TYPE_FILE: u32 = 3;

/// Shared max size for inbound/outbound media handling.
const WECHAT_MEDIA_MAX_BYTES: u64 = 100 * 1024 * 1024;

type Aes128EcbEnc = ecb::Encryptor<aes::Aes128>;
type Aes128EcbDec = ecb::Decryptor<aes::Aes128>;

fn long_poll_client_timeout(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms + 5_000)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WeChatAttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

impl WeChatAttachmentKind {
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

    fn default_extension(self) -> &'static str {
        match self {
            Self::Image => "png",
            Self::Document => "bin",
            Self::Video => "mp4",
            Self::Audio => "mp3",
            Self::Voice => "silk",
        }
    }

    fn upload_media_type(self) -> u32 {
        match self {
            Self::Image => UPLOAD_MEDIA_TYPE_IMAGE,
            Self::Video => UPLOAD_MEDIA_TYPE_VIDEO,
            Self::Document | Self::Audio | Self::Voice => UPLOAD_MEDIA_TYPE_FILE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WeChatAttachment {
    kind: WeChatAttachmentKind,
    target: String,
}

#[derive(Debug, Clone)]
struct WeChatMediaPayload {
    bytes: Vec<u8>,
    file_name: String,
}

#[derive(Debug, Clone)]
struct InboundAttachmentSpec {
    _kind: WeChatAttachmentKind,
    encrypted_query_param: String,
    aes_key: Option<String>,
    file_name: String,
}

#[derive(Debug, Clone)]
struct UploadedWeChatMedia {
    encrypted_query_param: String,
    aes_key_base64: String,
    raw_size: usize,
    encrypted_size: usize,
}

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

fn is_http_url(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://")
}

fn infer_attachment_kind_from_target(target: &str) -> Option<WeChatAttachmentKind> {
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
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => Some(WeChatAttachmentKind::Image),
        "mp4" | "mov" | "mkv" | "avi" | "webm" => Some(WeChatAttachmentKind::Video),
        "mp3" | "m4a" | "wav" | "flac" => Some(WeChatAttachmentKind::Audio),
        "ogg" | "oga" | "opus" | "silk" => Some(WeChatAttachmentKind::Voice),
        "pdf" | "txt" | "md" | "csv" | "json" | "zip" | "tar" | "gz" | "doc" | "docx" | "xls"
        | "xlsx" | "ppt" | "pptx" => Some(WeChatAttachmentKind::Document),
        _ => None,
    }
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

fn parse_attachment_markers(message: &str) -> (String, Vec<WeChatAttachment>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut attachments = Vec::new();
    let mut cursor = 0usize;

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
            let kind = WeChatAttachmentKind::from_marker(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(WeChatAttachment {
                kind,
                target: target.to_string(),
            })
        });

        if let Some(attachment) = parsed {
            attachments.push(attachment);
        } else {
            cleaned.push_str(&message[open..=close]);
        }

        cursor = close + 1;
    }

    (cleaned.trim().to_string(), attachments)
}

fn parse_path_only_attachment(message: &str) -> Option<WeChatAttachment> {
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

    Some(WeChatAttachment {
        kind,
        target: candidate.to_string(),
    })
}

fn format_attachment_content(local_filename: &str, local_path: &Path) -> String {
    if is_image_extension(local_path) {
        format!("[IMAGE:{}]", local_path.display())
    } else {
        format!("[Document: {}] {}", local_filename, local_path.display())
    }
}

fn sanitize_attachment_filename(file_name: &str) -> Option<String> {
    let cleaned = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())?
        .trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return None;
    }
    Some(cleaned.to_string())
}

fn aes_ecb_padded_size(plaintext_size: usize) -> usize {
    ((plaintext_size / 16) + 1) * 16
}

fn encrypt_aes_ecb(plaintext: &[u8], key: &[u8; 16]) -> anyhow::Result<Vec<u8>> {
    let padded_size = aes_ecb_padded_size(plaintext.len());
    let mut buffer = vec![0u8; padded_size];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let encrypted = Aes128EcbEnc::new(&(*key).into())
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext.len())
        .map_err(|e| anyhow::anyhow!("WeChat media encrypt failed: {e}"))?;
    Ok(encrypted.to_vec())
}

fn decrypt_aes_ecb(ciphertext: &[u8], key: &[u8; 16]) -> anyhow::Result<Vec<u8>> {
    let mut buffer = ciphertext.to_vec();
    Aes128EcbDec::new(&(*key).into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map(|decrypted| decrypted.to_vec())
        .map_err(|e| anyhow::anyhow!("WeChat media decrypt failed: {e}"))
}

fn parse_aes_key(raw: &str) -> anyhow::Result<[u8; 16]> {
    let raw = raw.trim();
    if raw.len() == 32 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = hex::decode(raw)
            .map_err(|e| anyhow::anyhow!("WeChat media hex aes_key invalid: {e}"))?;
        return <[u8; 16]>::try_from(bytes.as_slice())
            .map_err(|_| anyhow::anyhow!("WeChat media hex aes_key must be 16 bytes"));
    }

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| anyhow::anyhow!("WeChat media base64 aes_key invalid: {e}"))?;

    if decoded.len() == 16 {
        return <[u8; 16]>::try_from(decoded.as_slice())
            .map_err(|_| anyhow::anyhow!("WeChat media base64 aes_key must be 16 bytes"));
    }

    if decoded.len() == 32 && decoded.iter().all(u8::is_ascii_hexdigit) {
        let hex_text = std::str::from_utf8(&decoded)
            .map_err(|e| anyhow::anyhow!("WeChat media aes_key utf8 invalid: {e}"))?;
        let bytes = hex::decode(hex_text)
            .map_err(|e| anyhow::anyhow!("WeChat media nested hex aes_key invalid: {e}"))?;
        return <[u8; 16]>::try_from(bytes.as_slice())
            .map_err(|_| anyhow::anyhow!("WeChat media nested hex aes_key must be 16 bytes"));
    }

    anyhow::bail!(
        "WeChat media aes_key must decode to 16 raw bytes or 32 hex chars, got {} bytes",
        decoded.len()
    )
}

/// WeChat iLink Bot channel — long-polls the iLink Bot API for updates.
pub struct WeChatChannel {
    /// Bot token obtained via QR-code login; `None` until first login.
    bot_token: RwLock<Option<String>>,
    /// iLink bot ID (account ID); set after QR login.
    account_id: RwLock<Option<String>>,
    /// API base URL.
    api_base_url: String,
    /// CDN base URL.
    cdn_base_url: String,
    /// Allowed WeChat user IDs. Empty = deny all, `"*"` = allow all.
    allowed_users: Arc<RwLock<Vec<String>>>,
    /// Pairing guard for /bind flow.
    pairing: Option<PairingGuard>,
    /// HTTP client for API requests.
    client: reqwest::Client,
    /// Per-user context_token cache (accountId:userId -> token).
    context_tokens: Mutex<HashMap<String, String>>,
    /// Per-user typing_ticket cache (userId -> ticket).
    typing_tickets: Mutex<HashMap<String, String>>,
    /// Persisted getUpdates cursor.
    cursor: Mutex<String>,
    /// Typing indicator task handle.
    typing_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// State directory for persisting token & cursor.
    state_dir: PathBuf,
    /// Workspace directory used for storing inbound attachments and resolving
    /// `/workspace/...` paths from generated replies.
    workspace_dir: Option<PathBuf>,
}

/// Persistent account data (token + metadata).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct AccountData {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    saved_at: Option<String>,
}

/// Persistent sync cursor.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SyncData {
    #[serde(default)]
    get_updates_buf: String,
}

/// Write bytes to a file with owner-only permissions (0o600) on Unix.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Generate a random X-WECHAT-UIN header value.
fn random_wechat_uin() -> String {
    let bytes: [u8; 4] = rand::random();
    let uint32 = u32::from_be_bytes(bytes);
    base64::engine::general_purpose::STANDARD.encode(uint32.to_string())
}

fn build_base_info() -> serde_json::Value {
    serde_json::json!({
        "channel_version": env!("CARGO_PKG_VERSION")
    })
}

fn markdown_to_plain_text(text: &str) -> String {
    let code_block_re = regex::Regex::new(r"```[^\n]*\n?([\s\S]*?)```").unwrap();
    let image_re = regex::Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap();
    let link_re = regex::Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap();
    let heading_re = regex::Regex::new(r"(?m)^\s{0,3}#{1,6}\s+").unwrap();
    let blockquote_re = regex::Regex::new(r"(?m)^>\s?").unwrap();
    let bullet_re = regex::Regex::new(r"(?m)^\s*[-*+]\s+").unwrap();
    let emphasis_re = regex::Regex::new(r"(\*\*|__|~~|`|\*)").unwrap();
    let table_separator_re = regex::Regex::new(r"^\|[\s:|-]+\|$").unwrap();
    let table_row_re = regex::Regex::new(r"^\|(.+)\|$").unwrap();

    let mut result = code_block_re.replace_all(text, "$1").into_owned();
    result = image_re.replace_all(&result, "").into_owned();
    result = link_re.replace_all(&result, "$1").into_owned();

    let mut lines = Vec::new();
    for line in result.lines() {
        if table_separator_re.is_match(line) {
            continue;
        }

        if let Some(captures) = table_row_re.captures(line) {
            let inner = captures.get(1).map(|value| value.as_str()).unwrap_or("");
            lines.push(
                inner
                    .split('|')
                    .map(str::trim)
                    .filter(|cell| !cell.is_empty())
                    .collect::<Vec<_>>()
                    .join("  "),
            );
        } else {
            lines.push(line.to_string());
        }
    }

    result = lines.join("\n");
    result = heading_re.replace_all(&result, "").into_owned();
    result = blockquote_re.replace_all(&result, "").into_owned();
    result = bullet_re.replace_all(&result, "").into_owned();
    result = emphasis_re.replace_all(&result, "").into_owned();

    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

fn render_login_qr(code: &str) -> anyhow::Result<String> {
    let payload = code.trim();
    if payload.is_empty() {
        anyhow::bail!("WeChat QR payload is empty");
    }

    let qr = qrcode::QrCode::new(payload.as_bytes())
        .map_err(|err| anyhow::anyhow!("Failed to encode WeChat QR payload: {err}"))?;

    Ok(qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

/// Build common request headers for iLink API.
fn build_headers(token: Option<&str>) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Content-Type", "application/json".parse().unwrap());
    headers.insert("AuthorizationType", "ilink_bot_token".parse().unwrap());
    headers.insert("X-WECHAT-UIN", random_wechat_uin().parse().unwrap());
    if let Some(t) = token
        && !t.is_empty()
        && let Ok(val) = format!("Bearer {t}").parse()
    {
        headers.insert("Authorization", val);
    }
    headers
}

/// Extract text content from an iLink message's item_list.
fn extract_text_from_items(items: &[serde_json::Value]) -> String {
    for item in items {
        let item_type = item
            .get("type")
            .and_then(|v| v.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        match item_type {
            ITEM_TYPE_TEXT => {
                if let Some(text) = item
                    .get("text_item")
                    .and_then(|ti| ti.get("text"))
                    .and_then(|t| t.as_str())
                {
                    // Handle ref_msg (quoted message)
                    let ref_prefix = if let Some(ref_msg) = item.get("ref_msg") {
                        let title = ref_msg.get("title").and_then(|t| t.as_str()).unwrap_or("");
                        if title.is_empty() {
                            String::new()
                        } else {
                            format!("[引用: {title}]\n")
                        }
                    } else {
                        String::new()
                    };
                    return format!("{ref_prefix}{text}");
                }
            }
            ITEM_TYPE_VOICE => {
                // Voice-to-text transcription
                if let Some(text) = item
                    .get("voice_item")
                    .and_then(|vi| vi.get("text"))
                    .and_then(|t| t.as_str())
                    && !text.is_empty()
                {
                    return text.to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

impl WeChatChannel {
    pub fn new(
        allowed_users: Vec<String>,
        api_base_url: Option<String>,
        cdn_base_url: Option<String>,
        state_dir: Option<PathBuf>,
    ) -> Self {
        let pairing = if allowed_users.is_empty() {
            let guard = PairingGuard::new(true, &[]);
            if let Some(code) = guard.pairing_code() {
                println!("  🔐 WeChat pairing required. One-time bind code: {code}");
                println!("     Send `{WECHAT_BIND_COMMAND} <code>` from your WeChat.");
            }
            Some(guard)
        } else {
            None
        };

        let state_dir = state_dir.unwrap_or_else(|| {
            directories::UserDirs::new()
                .map(|u| u.home_dir().join(".zeroclaw").join("wechat"))
                .unwrap_or_else(|| PathBuf::from(".zeroclaw/wechat"))
        });

        let mut channel = Self {
            bot_token: RwLock::new(None),
            account_id: RwLock::new(None),
            api_base_url: api_base_url.unwrap_or_else(|| DEFAULT_API_BASE_URL.to_string()),
            cdn_base_url: cdn_base_url.unwrap_or_else(|| CDN_BASE_URL.to_string()),
            allowed_users: Arc::new(RwLock::new(allowed_users)),
            pairing,
            client: reqwest::Client::new(),
            context_tokens: Mutex::new(HashMap::new()),
            typing_tickets: Mutex::new(HashMap::new()),
            cursor: Mutex::new(String::new()),
            typing_handle: Mutex::new(None),
            state_dir,
            workspace_dir: None,
        };

        // Try to load persisted state
        channel.load_persisted_state();
        channel
    }

    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Load persisted token and cursor from state_dir.
    fn load_persisted_state(&mut self) {
        let account_path = self.state_dir.join("account.json");
        if let Ok(data) = std::fs::read_to_string(&account_path)
            && let Ok(account) = serde_json::from_str::<AccountData>(&data)
        {
            if let Some(ref token) = account.token
                && !token.is_empty()
            {
                *self.bot_token.write().unwrap() = Some(token.clone());
                tracing::info!("WeChat: loaded persisted bot token");
            }
            if let Some(ref id) = account.account_id {
                *self.account_id.write().unwrap() = Some(id.clone());
            }
        }

        let sync_path = self.state_dir.join("sync.json");
        if let Ok(data) = std::fs::read_to_string(&sync_path)
            && let Ok(sync) = serde_json::from_str::<SyncData>(&data)
            && !sync.get_updates_buf.is_empty()
        {
            *self.cursor.lock() = sync.get_updates_buf;
            tracing::info!("WeChat: loaded persisted sync cursor");
        }
    }

    /// Save account data to disk.
    fn save_account_data(&self, token: &str, account_id: &str, user_id: Option<&str>) {
        if let Err(e) = std::fs::create_dir_all(&self.state_dir) {
            tracing::warn!("WeChat: failed to create state dir: {e}");
            return;
        }
        let data = AccountData {
            token: Some(token.to_string()),
            account_id: Some(account_id.to_string()),
            base_url: Some(self.api_base_url.clone()),
            user_id: user_id.map(String::from),
            saved_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        let path = self.state_dir.join("account.json");
        match serde_json::to_string_pretty(&data) {
            Ok(json) => {
                if let Err(e) = write_private(&path, json.as_bytes()) {
                    tracing::warn!("WeChat: failed to write account data: {e}");
                }
            }
            Err(e) => tracing::warn!("WeChat: failed to serialize account data: {e}"),
        }
    }

    /// Save sync cursor to disk.
    fn save_cursor(&self, cursor: &str) {
        if let Err(e) = std::fs::create_dir_all(&self.state_dir) {
            tracing::warn!("WeChat: failed to create state dir: {e}");
            return;
        }
        let data = SyncData {
            get_updates_buf: cursor.to_string(),
        };
        let path = self.state_dir.join("sync.json");
        match serde_json::to_string(&data) {
            Ok(json) => {
                if let Err(e) = write_private(&path, json.as_bytes()) {
                    tracing::warn!("WeChat: failed to write sync data: {e}");
                }
            }
            Err(e) => tracing::warn!("WeChat: failed to serialize sync data: {e}"),
        }
    }

    fn has_token(&self) -> bool {
        self.bot_token.read().map(|t| t.is_some()).unwrap_or(false)
    }

    fn get_token(&self) -> Option<String> {
        self.bot_token.read().ok().and_then(|t| t.clone())
    }

    fn set_context_token(&self, user_id: &str, token: &str) {
        self.context_tokens
            .lock()
            .insert(user_id.to_string(), token.to_string());
    }

    fn get_context_token(&self, user_id: &str) -> Option<String> {
        self.context_tokens.lock().get(user_id).cloned()
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users
            .read()
            .map(|users| users.iter().any(|u| u == "*" || u == user_id))
            .unwrap_or(false)
    }

    fn add_allowed_identity_runtime(&self, identity: &str) {
        if identity.is_empty() {
            return;
        }
        if let Ok(mut users) = self.allowed_users.write()
            && !users.iter().any(|u| u == identity)
        {
            users.push(identity.to_string());
        }
    }

    fn extract_bind_code(text: &str) -> Option<&str> {
        let mut parts = text.split_whitespace();
        let command = parts.next()?;
        if command != WECHAT_BIND_COMMAND {
            return None;
        }
        parts.next().map(str::trim).filter(|code| !code.is_empty())
    }

    fn api_url(&self, endpoint: &str) -> String {
        let base = self.api_base_url.trim_end_matches('/');
        format!("{base}/ilink/bot/{endpoint}")
    }

    fn cdn_download_url(&self, encrypted_query_param: &str) -> String {
        let base = self.cdn_base_url.trim_end_matches('/');
        format!(
            "{base}/download?encrypted_query_param={}",
            urlencoding::encode(encrypted_query_param)
        )
    }

    fn cdn_upload_url(&self, upload_param: &str, filekey: &str) -> String {
        let base = self.cdn_base_url.trim_end_matches('/');
        format!(
            "{base}/upload?encrypted_query_param={}&filekey={}",
            urlencoding::encode(upload_param),
            urlencoding::encode(filekey)
        )
    }

    fn resolve_local_attachment_path(&self, target: &str) -> PathBuf {
        let target = target.trim();
        let target = target.strip_prefix("file://").unwrap_or(target);

        let resolved = if let Some(rel) = target.strip_prefix("/workspace/") {
            if let Some(workspace_dir) = &self.workspace_dir {
                workspace_dir.join(rel)
            } else {
                PathBuf::from(target)
            }
        } else {
            let path = PathBuf::from(target);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(path)
            }
        };

        // Prevent path traversal outside workspace when workspace_dir is set
        if let Some(workspace_dir) = &self.workspace_dir
            && let (Ok(canonical), Ok(allowed)) =
                (resolved.canonicalize(), workspace_dir.canonicalize())
            && !canonical.starts_with(&allowed)
        {
            tracing::warn!(
                "WeChat: attachment path {} escapes workspace {}, rejected",
                canonical.display(),
                allowed.display()
            );
            return PathBuf::from(format!(
                "/nonexistent/blocked_path_traversal_{}",
                uuid::Uuid::new_v4()
            ));
        }

        resolved
    }

    fn remote_file_name(
        &self,
        url: &str,
        content_type: Option<&str>,
        kind: WeChatAttachmentKind,
    ) -> String {
        let cleaned_url = url
            .split('?')
            .next()
            .unwrap_or(url)
            .split('#')
            .next()
            .unwrap_or(url);

        if let Some(last_segment) = cleaned_url.rsplit('/').next()
            && let Some(name) = sanitize_attachment_filename(last_segment)
            && Path::new(&name).extension().is_some()
        {
            return name;
        }

        let ext = content_type
            .and_then(|value| value.split(';').next())
            .and_then(mime_guess::get_mime_extensions_str)
            .and_then(|exts: &[&str]| exts.first().copied())
            .unwrap_or(kind.default_extension());

        format!(
            "wechat_attachment_{}.{}",
            uuid::Uuid::new_v4().simple(),
            ext
        )
    }

    async fn download_remote_attachment(
        &self,
        url: &str,
        kind: WeChatAttachmentKind,
    ) -> anyhow::Result<WeChatMediaPayload> {
        if !url.starts_with("https://") {
            anyhow::bail!("WeChat: refusing non-HTTPS attachment URL: {url}");
        }
        let resp = self
            .client
            .get(url)
            .timeout(API_TIMEOUT)
            .send()
            .await
            .with_context(|| format!("WeChat attachment download failed: {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("WeChat attachment download failed ({status}): {body}");
        }

        if let Some(len) = resp.content_length()
            && len > WECHAT_MEDIA_MAX_BYTES
        {
            anyhow::bail!(
                "WeChat attachment Content-Length ({len} bytes) exceeds {} MB limit",
                WECHAT_MEDIA_MAX_BYTES / (1024 * 1024)
            );
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = resp.bytes().await?.to_vec();

        if bytes.len() as u64 > WECHAT_MEDIA_MAX_BYTES {
            anyhow::bail!(
                "WeChat attachment exceeds {} MB limit",
                WECHAT_MEDIA_MAX_BYTES / (1024 * 1024)
            );
        }

        Ok(WeChatMediaPayload {
            file_name: self.remote_file_name(url, content_type.as_deref(), kind),
            bytes,
        })
    }

    async fn load_attachment_payload(
        &self,
        attachment: &WeChatAttachment,
    ) -> anyhow::Result<WeChatMediaPayload> {
        let target = attachment.target.trim();
        if is_http_url(target) {
            return self
                .download_remote_attachment(target, attachment.kind)
                .await;
        }

        let path = self.resolve_local_attachment_path(target);
        if !path.exists() {
            anyhow::bail!("WeChat attachment path not found: {}", path.display());
        }

        let file_name = sanitize_attachment_filename(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("attachment.bin"),
        )
        .unwrap_or_else(|| {
            format!(
                "wechat_attachment_{}.{}",
                uuid::Uuid::new_v4().simple(),
                attachment.kind.default_extension()
            )
        });

        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("WeChat attachment read failed: {}", path.display()))?;
        if bytes.len() as u64 > WECHAT_MEDIA_MAX_BYTES {
            anyhow::bail!(
                "WeChat attachment exceeds {} MB limit",
                WECHAT_MEDIA_MAX_BYTES / (1024 * 1024)
            );
        }

        Ok(WeChatMediaPayload { bytes, file_name })
    }

    async fn request_upload_param(
        &self,
        to: &str,
        kind: WeChatAttachmentKind,
        payload: &WeChatMediaPayload,
        aes_key: &[u8; 16],
        filekey: &str,
    ) -> anyhow::Result<String> {
        let token = self
            .get_token()
            .context("WeChat: not logged in, cannot upload attachment")?;
        let body = serde_json::json!({
            "filekey": filekey,
            "media_type": kind.upload_media_type(),
            "to_user_id": to,
            "rawsize": payload.bytes.len(),
            "rawfilemd5": format!("{:x}", md5::compute(&payload.bytes)),
            "filesize": aes_ecb_padded_size(payload.bytes.len()),
            "no_need_thumb": true,
            "aeskey": hex::encode(aes_key),
            "base_info": build_base_info()
        });

        let resp = self
            .client
            .post(self.api_url("getuploadurl"))
            .headers(build_headers(Some(&token)))
            .json(&body)
            .timeout(API_TIMEOUT)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("WeChat getUploadUrl failed ({status}): {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        data.get("upload_param")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .context("WeChat getUploadUrl returned no upload_param")
    }

    async fn upload_to_cdn(
        &self,
        upload_param: &str,
        filekey: &str,
        ciphertext: &[u8],
    ) -> anyhow::Result<String> {
        let url = self.cdn_upload_url(upload_param, filekey);
        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 1..=3 {
            let resp = self
                .client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(ciphertext.to_vec())
                .timeout(API_TIMEOUT)
                .send()
                .await;

            match resp {
                Ok(resp) if resp.status().is_success() => {
                    let encrypted_param = resp
                        .headers()
                        .get("x-encrypted-param")
                        .and_then(|value| value.to_str().ok())
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .context("WeChat CDN upload missing x-encrypted-param header")?;
                    return Ok(encrypted_param);
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let error = anyhow::anyhow!(
                        "WeChat CDN upload failed on attempt {attempt} ({status}): {body}"
                    );
                    if status.is_client_error() {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
                Err(err) => {
                    last_error = Some(anyhow::anyhow!(
                        "WeChat CDN upload request failed on attempt {attempt}: {err}"
                    ));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("WeChat CDN upload failed")))
    }

    async fn upload_media_payload(
        &self,
        to: &str,
        kind: WeChatAttachmentKind,
        payload: &WeChatMediaPayload,
    ) -> anyhow::Result<UploadedWeChatMedia> {
        let filekey = uuid::Uuid::new_v4().simple().to_string();
        let aes_key: [u8; 16] = rand::random();
        let upload_param = self
            .request_upload_param(to, kind, payload, &aes_key, &filekey)
            .await?;
        let ciphertext = encrypt_aes_ecb(&payload.bytes, &aes_key)?;
        let encrypted_query_param = self
            .upload_to_cdn(&upload_param, &filekey, &ciphertext)
            .await?;

        Ok(UploadedWeChatMedia {
            encrypted_query_param,
            aes_key_base64: base64::engine::general_purpose::STANDARD.encode(aes_key),
            raw_size: payload.bytes.len(),
            encrypted_size: ciphertext.len(),
        })
    }

    fn find_inbound_attachment(
        items: &[serde_json::Value],
        message_id: &str,
    ) -> Option<InboundAttachmentSpec> {
        fn default_name(kind: WeChatAttachmentKind, message_id: &str) -> String {
            let safe_id: String = message_id
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect();
            match kind {
                WeChatAttachmentKind::Image => format!("wechat_{safe_id}.jpg"),
                WeChatAttachmentKind::Document => format!("wechat_{safe_id}.bin"),
                WeChatAttachmentKind::Video => format!("wechat_{safe_id}.mp4"),
                WeChatAttachmentKind::Audio => format!("wechat_{safe_id}.mp3"),
                WeChatAttachmentKind::Voice => format!("wechat_{safe_id}.silk"),
            }
        }

        fn parse_item(item: &serde_json::Value, message_id: &str) -> Option<InboundAttachmentSpec> {
            let item_type = item
                .get("type")
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())?;
            match item_type {
                ITEM_TYPE_IMAGE => {
                    let image_item = item.get("image_item")?;
                    let media = image_item.get("media")?;
                    let encrypted_query_param =
                        media.get("encrypt_query_param")?.as_str()?.to_string();
                    let aes_key = image_item
                        .get("aeskey")
                        .and_then(|value| value.as_str())
                        .or_else(|| media.get("aes_key").and_then(|value| value.as_str()))
                        .map(str::to_string);
                    Some(InboundAttachmentSpec {
                        _kind: WeChatAttachmentKind::Image,
                        encrypted_query_param,
                        aes_key,
                        file_name: default_name(WeChatAttachmentKind::Image, message_id),
                    })
                }
                ITEM_TYPE_FILE => {
                    let file_item = item.get("file_item")?;
                    let media = file_item.get("media")?;
                    let encrypted_query_param =
                        media.get("encrypt_query_param")?.as_str()?.to_string();
                    let aes_key = media
                        .get("aes_key")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let file_name = file_item
                        .get("file_name")
                        .and_then(|value| value.as_str())
                        .and_then(sanitize_attachment_filename)
                        .unwrap_or_else(|| {
                            default_name(WeChatAttachmentKind::Document, message_id)
                        });
                    Some(InboundAttachmentSpec {
                        _kind: WeChatAttachmentKind::Document,
                        encrypted_query_param,
                        aes_key,
                        file_name,
                    })
                }
                ITEM_TYPE_VIDEO => {
                    let video_item = item.get("video_item")?;
                    let media = video_item.get("media")?;
                    let encrypted_query_param =
                        media.get("encrypt_query_param")?.as_str()?.to_string();
                    let aes_key = media
                        .get("aes_key")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    Some(InboundAttachmentSpec {
                        _kind: WeChatAttachmentKind::Video,
                        encrypted_query_param,
                        aes_key,
                        file_name: default_name(WeChatAttachmentKind::Video, message_id),
                    })
                }
                ITEM_TYPE_VOICE => {
                    let voice_item = item.get("voice_item")?;
                    let media = voice_item.get("media")?;
                    let encrypted_query_param =
                        media.get("encrypt_query_param")?.as_str()?.to_string();
                    let aes_key = media
                        .get("aes_key")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    Some(InboundAttachmentSpec {
                        _kind: WeChatAttachmentKind::Voice,
                        encrypted_query_param,
                        aes_key,
                        file_name: default_name(WeChatAttachmentKind::Voice, message_id),
                    })
                }
                _ => None,
            }
        }

        for item in items {
            if let Some(spec) = parse_item(item, message_id) {
                return Some(spec);
            }
        }

        for item in items {
            let Some(ref_item) = item
                .get("ref_msg")
                .and_then(|value| value.get("message_item"))
            else {
                continue;
            };

            if let Some(spec) = parse_item(ref_item, message_id) {
                return Some(spec);
            }
        }

        None
    }

    async fn download_inbound_attachment(
        &self,
        spec: &InboundAttachmentSpec,
    ) -> anyhow::Result<Vec<u8>> {
        let resp = self
            .client
            .get(self.cdn_download_url(&spec.encrypted_query_param))
            .timeout(API_TIMEOUT)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("WeChat attachment download failed ({status}): {body}");
        }

        let bytes = resp.bytes().await?.to_vec();
        if bytes.len() as u64 > WECHAT_MEDIA_MAX_BYTES {
            anyhow::bail!(
                "WeChat inbound attachment exceeds {} MB limit",
                WECHAT_MEDIA_MAX_BYTES / (1024 * 1024)
            );
        }

        match spec.aes_key.as_deref() {
            Some(aes_key) if !aes_key.is_empty() => {
                let key = parse_aes_key(aes_key)?;
                decrypt_aes_ecb(&bytes, &key)
            }
            _ => Ok(bytes),
        }
    }

    async fn try_build_attachment_content(
        &self,
        items: &[serde_json::Value],
        message_id: &str,
    ) -> Option<String> {
        let workspace_dir = self.workspace_dir.as_ref()?;
        let spec = Self::find_inbound_attachment(items, message_id)?;
        let bytes = match self.download_inbound_attachment(&spec).await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!("WeChat attachment download skipped: {err}");
                return None;
            }
        };

        let save_dir = workspace_dir.join("wechat_files");
        if let Err(err) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("Failed to create WeChat attachment dir: {err}");
            return None;
        }

        let local_path = save_dir.join(&spec.file_name);
        if let Err(err) = tokio::fs::write(&local_path, bytes).await {
            tracing::warn!(
                "Failed to save WeChat attachment to {}: {err}",
                local_path.display()
            );
            return None;
        }

        Some(format_attachment_content(&spec.file_name, &local_path))
    }

    /// Perform QR-code login flow. Returns (bot_token, account_id, user_id).
    async fn qr_login(&self) -> anyhow::Result<(String, String, Option<String>)> {
        let mut qr_refresh_count = 0u32;

        loop {
            qr_refresh_count += 1;
            if qr_refresh_count > MAX_QR_REFRESH {
                anyhow::bail!("WeChat: QR code expired {MAX_QR_REFRESH} times, giving up");
            }

            // Fetch QR code
            let qr_url = format!("{}?bot_type=3", self.api_url("get_bot_qrcode"));
            let resp = self
                .client
                .get(&qr_url)
                .timeout(API_TIMEOUT)
                .send()
                .await
                .context("Failed to fetch WeChat QR code")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("WeChat QR code fetch failed ({status}): {body}");
            }

            let qr_data: serde_json::Value = resp.json().await?;
            let qrcode = qr_data
                .get("qrcode")
                .and_then(|v| v.as_str())
                .context("Missing qrcode in response")?
                .to_string();
            let qrcode_img_url = qr_data
                .get("qrcode_img_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Display QR code
            println!("\n  📱 WeChat QR Login ({qr_refresh_count}/{MAX_QR_REFRESH})");
            println!("  Scan with WeChat to connect.\n");
            let qr_payload = if qrcode_img_url.is_empty() {
                qrcode.as_str()
            } else {
                qrcode_img_url
            };
            match render_login_qr(qr_payload) {
                Ok(qr) => println!("{qr}"),
                Err(err) => tracing::warn!("WeChat: failed to render terminal QR code: {err}"),
            }
            if !qrcode_img_url.is_empty() {
                println!("  QR URL: {qrcode_img_url}");
            }

            // Poll for scan status
            let deadline = std::time::Instant::now() + QR_SCAN_TIMEOUT;
            let mut scanned_printed = false;

            while std::time::Instant::now() < deadline {
                let status_url = format!(
                    "{}?qrcode={}",
                    self.api_url("get_qrcode_status"),
                    urlencoding::encode(&qrcode)
                );
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert("iLink-App-ClientVersion", "1".parse().unwrap());

                let poll_result = tokio::time::timeout(
                    QR_POLL_TIMEOUT + Duration::from_secs(5),
                    self.client
                        .get(&status_url)
                        .headers(headers)
                        .timeout(QR_POLL_TIMEOUT)
                        .send(),
                )
                .await;

                let resp = match poll_result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::debug!("WeChat QR poll error: {e}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    Err(_) => {
                        // Client-side timeout, normal for long-poll
                        continue;
                    }
                };

                let status: serde_json::Value = match resp.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("WeChat QR poll parse error: {e}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                let status_str = status
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("wait");

                match status_str {
                    "wait" => {}
                    "scaned" => {
                        if !scanned_printed {
                            println!("  👀 Scanned! Confirm on your phone...");
                            scanned_printed = true;
                        }
                    }
                    "expired" => {
                        println!("  ⏳ QR code expired, refreshing...");
                        break; // Will loop back and get a new QR code
                    }
                    "confirmed" => {
                        let bot_token = status
                            .get("bot_token")
                            .and_then(|v| v.as_str())
                            .context("Login confirmed but bot_token missing")?
                            .to_string();
                        let account_id = status
                            .get("ilink_bot_id")
                            .and_then(|v| v.as_str())
                            .context("Login confirmed but ilink_bot_id missing")?
                            .to_string();
                        let user_id = status
                            .get("ilink_user_id")
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        println!("  ✅ WeChat connected!");
                        return Ok((bot_token, account_id, user_id));
                    }
                    other => {
                        tracing::debug!("WeChat QR status: {other}");
                    }
                }

                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            // If we reach here without returning, the QR expired or timed out.
            // Loop will try again up to MAX_QR_REFRESH times.
        }
    }

    /// Ensure we have a valid bot token, performing QR login if needed.
    async fn ensure_logged_in(&self) -> anyhow::Result<()> {
        if self.has_token() {
            return Ok(());
        }

        tracing::info!("WeChat: no persisted token, starting QR login...");
        let (token, account_id, user_id) = self.qr_login().await?;

        // Save to memory
        if let Ok(mut t) = self.bot_token.write() {
            *t = Some(token.clone());
        }
        if let Ok(mut a) = self.account_id.write() {
            *a = Some(account_id.clone());
        }

        // If a user scanned, auto-add them to allowed list
        if let Some(ref uid) = user_id {
            self.add_allowed_identity_runtime(uid);
        }

        // Persist to disk
        self.save_account_data(&token, &account_id, user_id.as_deref());

        Ok(())
    }

    async fn send_message_items(
        &self,
        to: &str,
        item_list: Vec<serde_json::Value>,
        context_token: Option<&str>,
    ) -> anyhow::Result<()> {
        let token = self
            .get_token()
            .context("WeChat: not logged in, cannot send")?;

        let client_id = format!("zeroclaw-{}", uuid::Uuid::new_v4());
        let body = serde_json::json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to,
                "client_id": client_id,
                "message_type": MESSAGE_TYPE_BOT,
                "message_state": MESSAGE_STATE_FINISH,
                "item_list": item_list,
                "context_token": context_token.unwrap_or("")
            },
            "base_info": build_base_info()
        });

        let resp = self
            .client
            .post(self.api_url("sendmessage"))
            .headers(build_headers(Some(&token)))
            .json(&body)
            .timeout(API_TIMEOUT)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("WeChat sendMessage failed ({status}): {err}");
        }

        Ok(())
    }

    /// Send a text message via iLink API.
    async fn send_text(
        &self,
        to: &str,
        text: &str,
        context_token: Option<&str>,
    ) -> anyhow::Result<()> {
        self.send_message_items(
            to,
            vec![serde_json::json!({
                "type": ITEM_TYPE_TEXT,
                "text_item": { "text": markdown_to_plain_text(text) }
            })],
            context_token,
        )
        .await
    }

    async fn send_attachment(
        &self,
        to: &str,
        attachment: &WeChatAttachment,
        context_token: Option<&str>,
    ) -> anyhow::Result<()> {
        let payload = self.load_attachment_payload(attachment).await?;
        let uploaded = self
            .upload_media_payload(to, attachment.kind, &payload)
            .await?;

        let item = match attachment.kind {
            WeChatAttachmentKind::Image => serde_json::json!({
                "type": ITEM_TYPE_IMAGE,
                "image_item": {
                    "media": {
                        "encrypt_query_param": uploaded.encrypted_query_param,
                        "aes_key": uploaded.aes_key_base64,
                        "encrypt_type": 1
                    },
                    "mid_size": uploaded.encrypted_size
                }
            }),
            WeChatAttachmentKind::Video => serde_json::json!({
                "type": ITEM_TYPE_VIDEO,
                "video_item": {
                    "media": {
                        "encrypt_query_param": uploaded.encrypted_query_param,
                        "aes_key": uploaded.aes_key_base64,
                        "encrypt_type": 1
                    },
                    "video_size": uploaded.encrypted_size
                }
            }),
            WeChatAttachmentKind::Document
            | WeChatAttachmentKind::Audio
            | WeChatAttachmentKind::Voice => serde_json::json!({
                "type": ITEM_TYPE_FILE,
                "file_item": {
                    "media": {
                        "encrypt_query_param": uploaded.encrypted_query_param,
                        "aes_key": uploaded.aes_key_base64,
                        "encrypt_type": 1
                    },
                    "file_name": payload.file_name,
                    "len": uploaded.raw_size.to_string()
                }
            }),
        };

        self.send_message_items(to, vec![item], context_token).await
    }

    /// Fetch typing_ticket for a user via getconfig.
    async fn fetch_typing_ticket(&self, user_id: &str) -> Option<String> {
        let token = self.get_token()?;
        let context_token = self.get_context_token(user_id);

        let body = serde_json::json!({
            "ilink_user_id": user_id,
            "context_token": context_token.unwrap_or_default(),
            "base_info": build_base_info()
        });

        let resp = self
            .client
            .post(self.api_url("getconfig"))
            .headers(build_headers(Some(&token)))
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .ok()?;

        let data: serde_json::Value = resp.json().await.ok()?;
        data.get("typing_ticket")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// Get or fetch typing_ticket for a user.
    async fn get_typing_ticket(&self, user_id: &str) -> Option<String> {
        // Check cache first
        if let Some(ticket) = self.typing_tickets.lock().get(user_id).cloned() {
            return Some(ticket);
        }

        // Fetch and cache
        let ticket = self.fetch_typing_ticket(user_id).await?;
        self.typing_tickets
            .lock()
            .insert(user_id.to_string(), ticket.clone());
        Some(ticket)
    }

    /// Handle an unauthorized message (check for /bind command).
    async fn handle_unauthorized_message(&self, from_user_id: &str, text: &str) {
        if let Some(code) = Self::extract_bind_code(text) {
            if let Some(pairing) = self.pairing.as_ref() {
                match pairing.try_pair(code, from_user_id).await {
                    Ok(Some(_token)) => {
                        self.add_allowed_identity_runtime(from_user_id);
                        let ctx = self.get_context_token(from_user_id);
                        let _ = self
                            .send_text(
                                from_user_id,
                                "✅ WeChat account bound successfully. You can talk to ZeroClaw now.",
                                ctx.as_deref(),
                            )
                            .await;
                        tracing::info!("WeChat: user {from_user_id} bound via pairing code");
                    }
                    Ok(None) => {
                        let ctx = self.get_context_token(from_user_id);
                        let _ = self
                            .send_text(
                                from_user_id,
                                "❌ Invalid bind code. Please try again.",
                                ctx.as_deref(),
                            )
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!("WeChat: pairing error: {e}");
                    }
                }
            }
        } else {
            tracing::debug!("WeChat: ignoring unauthorized message from {from_user_id}");
        }
    }
}

#[async_trait]
impl Channel for WeChatChannel {
    fn name(&self) -> &str {
        "wechat"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let recipient = &message.recipient;
        let content = crate::util::strip_tool_call_tags(&message.content);
        let context_token = self.get_context_token(recipient);

        if context_token.is_none() {
            tracing::warn!(
                "WeChat: no context_token for {recipient}, message may fail to associate"
            );
        }

        let (text_without_markers, attachments) = parse_attachment_markers(&content);
        if !attachments.is_empty() {
            if !text_without_markers.is_empty() {
                self.send_text(recipient, &text_without_markers, context_token.as_deref())
                    .await?;
            }

            for attachment in &attachments {
                self.send_attachment(recipient, attachment, context_token.as_deref())
                    .await?;
            }
            return Ok(());
        }

        if let Some(attachment) = parse_path_only_attachment(&content) {
            return self
                .send_attachment(recipient, &attachment, context_token.as_deref())
                .await;
        }

        self.send_text(recipient, &content, context_token.as_deref())
            .await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Ensure we're logged in (QR scan if needed)
        self.ensure_logged_in().await?;

        tracing::info!("WeChat channel listening for messages...");

        let mut cursor = self.cursor.lock().clone();
        let mut long_poll_timeout_ms = LONG_POLL_TIMEOUT_MS;
        let mut consecutive_failures: u32 = 0;

        loop {
            let token = match self.get_token() {
                Some(t) => t,
                None => {
                    tracing::error!("WeChat: token lost, attempting re-login...");
                    if let Err(e) = self.ensure_logged_in().await {
                        tracing::error!("WeChat: re-login failed: {e}");
                        tokio::time::sleep(BACKOFF_DELAY).await;
                        continue;
                    }
                    match self.get_token() {
                        Some(t) => t,
                        None => {
                            tokio::time::sleep(BACKOFF_DELAY).await;
                            continue;
                        }
                    }
                }
            };

            let body = serde_json::json!({
                "get_updates_buf": cursor,
                "base_info": build_base_info()
            });

            let result = tokio::time::timeout(
                long_poll_client_timeout(long_poll_timeout_ms),
                self.client
                    .post(self.api_url("getupdates"))
                    .headers(build_headers(Some(&token)))
                    .json(&body)
                    .timeout(Duration::from_millis(long_poll_timeout_ms))
                    .send(),
            )
            .await;

            let resp = match result {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    consecutive_failures += 1;
                    tracing::warn!(
                        "WeChat getUpdates error ({consecutive_failures}/{MAX_CONSECUTIVE_FAILURES}): {e}"
                    );
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        consecutive_failures = 0;
                        tokio::time::sleep(BACKOFF_DELAY).await;
                    } else {
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                    continue;
                }
                Err(_) => {
                    // Client-side timeout — normal for long-poll, just retry
                    tracing::debug!("WeChat getUpdates: client-side timeout, retrying");
                    continue;
                }
            };

            let data: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    consecutive_failures += 1;
                    tracing::warn!("WeChat getUpdates parse error: {e}");
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        consecutive_failures = 0;
                        tokio::time::sleep(BACKOFF_DELAY).await;
                    } else {
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                    continue;
                }
            };

            // Check for API errors
            let ret = data.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
            let errcode = data.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
            let is_error = ret != 0 || errcode != 0;

            if is_error {
                if errcode == SESSION_EXPIRED_ERRCODE || ret == SESSION_EXPIRED_ERRCODE {
                    tracing::error!(
                        "WeChat: session expired (errcode {SESSION_EXPIRED_ERRCODE}), pausing for {} min",
                        SESSION_PAUSE_DURATION.as_secs() / 60
                    );
                    // Clear token so we re-login after pause
                    if let Ok(mut t) = self.bot_token.write() {
                        *t = None;
                    }
                    tokio::time::sleep(SESSION_PAUSE_DURATION).await;
                    // Try to re-login
                    if let Err(e) = self.ensure_logged_in().await {
                        tracing::error!("WeChat: re-login after session expiry failed: {e}");
                    }
                    consecutive_failures = 0;
                    continue;
                }

                consecutive_failures += 1;
                let errmsg = data.get("errmsg").and_then(|v| v.as_str()).unwrap_or("");
                tracing::warn!(
                    "WeChat getUpdates failed: ret={ret} errcode={errcode} errmsg={errmsg} ({consecutive_failures}/{MAX_CONSECUTIVE_FAILURES})"
                );
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    consecutive_failures = 0;
                    tokio::time::sleep(BACKOFF_DELAY).await;
                } else {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                continue;
            }

            consecutive_failures = 0;

            // Update cursor
            if let Some(new_cursor) = data
                .get("get_updates_buf")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                cursor = new_cursor.to_string();
                *self.cursor.lock() = cursor.clone();
                self.save_cursor(&cursor);
            }

            if let Some(next_timeout) = data
                .get("longpolling_timeout_ms")
                .and_then(|v| v.as_u64())
                .filter(|timeout| *timeout > 0)
            {
                long_poll_timeout_ms = next_timeout;
            }

            // Process messages
            let msgs = data
                .get("msgs")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            for msg in &msgs {
                let from_user_id = msg
                    .get("from_user_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if from_user_id.is_empty() {
                    continue;
                }

                // Cache context_token
                if let Some(ctx_token) = msg.get("context_token").and_then(|v| v.as_str())
                    && !ctx_token.is_empty()
                {
                    self.set_context_token(from_user_id, ctx_token);
                }

                let items = msg
                    .get("item_list")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                let message_id = msg
                    .get("message_id")
                    .and_then(|v| v.as_u64())
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| format!("wechat_{}", uuid::Uuid::new_v4()));

                let text = extract_text_from_items(&items);

                // Check authorization
                if !self.is_user_allowed(from_user_id) {
                    self.handle_unauthorized_message(from_user_id, &text).await;
                    continue;
                }

                let attachment_content =
                    self.try_build_attachment_content(&items, &message_id).await;
                let content = match (attachment_content, text.is_empty()) {
                    (Some(marker), true) => marker,
                    (Some(marker), false) => format!("{marker}\n\n{text}"),
                    (None, false) => text,
                    (None, true) => continue,
                };

                let timestamp = msg
                    .get("create_time_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    / 1000; // Convert to seconds

                let channel_msg = ChannelMessage {
                    id: message_id,
                    sender: from_user_id.to_string(),
                    reply_target: from_user_id.to_string(),
                    content,
                    channel: "wechat".to_string(),
                    timestamp,
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: Vec::new(),
                };

                if tx.send(channel_msg).await.is_err() {
                    tracing::info!("WeChat: channel receiver dropped, stopping");
                    return Ok(());
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let token = match self.get_token() {
            Some(t) => t,
            None => return false,
        };

        // Use getconfig with a dummy user as a health check
        let body = serde_json::json!({
            "ilink_user_id": "",
            "context_token": "",
            "base_info": build_base_info()
        });

        match tokio::time::timeout(
            Duration::from_secs(5),
            self.client
                .post(self.api_url("getconfig"))
                .headers(build_headers(Some(&token)))
                .json(&body)
                .send(),
        )
        .await
        {
            Ok(Ok(resp)) => resp.status().is_success(),
            _ => false,
        }
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.stop_typing(recipient).await?;

        let token = match self.get_token() {
            Some(t) => t,
            None => return Ok(()),
        };

        let typing_ticket = match self.get_typing_ticket(recipient).await {
            Some(t) => t,
            None => return Ok(()),
        };

        let client = self.client.clone();
        let url = self.api_url("sendtyping");
        let user_id = recipient.to_string();

        let handle = tokio::spawn(async move {
            loop {
                let body = serde_json::json!({
                    "ilink_user_id": &user_id,
                    "typing_ticket": &typing_ticket,
                    "status": 1,
                    "base_info": build_base_info()
                });
                let _ = client
                    .post(&url)
                    .headers(build_headers(Some(&token)))
                    .json(&body)
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await;
                // Refresh typing indicator every 4 seconds
                tokio::time::sleep(Duration::from_secs(4)).await;
            }
        });

        *self.typing_handle.lock() = Some(handle);
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
    use tempfile::tempdir;

    #[test]
    fn wechat_channel_name() {
        let ch = WeChatChannel::new(
            vec!["*".into()],
            None,
            None,
            Some("/tmp/test-wechat".into()),
        );
        assert_eq!(ch.name(), "wechat");
    }

    #[test]
    fn extract_text_from_items_text() {
        let items = vec![serde_json::json!({
            "type": 1,
            "text_item": { "text": "hello world" }
        })];
        assert_eq!(extract_text_from_items(&items), "hello world");
    }

    #[test]
    fn extract_text_from_items_voice() {
        let items = vec![serde_json::json!({
            "type": 3,
            "voice_item": { "text": "voice transcription" }
        })];
        assert_eq!(extract_text_from_items(&items), "voice transcription");
    }

    #[test]
    fn extract_text_from_items_empty() {
        let items = vec![serde_json::json!({
            "type": 2,
            "image_item": {}
        })];
        assert_eq!(extract_text_from_items(&items), "");
    }

    #[test]
    fn extract_bind_code_valid() {
        assert_eq!(
            WeChatChannel::extract_bind_code("/bind ABC123"),
            Some("ABC123")
        );
    }

    #[test]
    fn extract_bind_code_no_code() {
        assert_eq!(WeChatChannel::extract_bind_code("/bind"), None);
    }

    #[test]
    fn extract_bind_code_wrong_command() {
        assert_eq!(WeChatChannel::extract_bind_code("/start"), None);
    }

    #[test]
    fn is_user_allowed_wildcard() {
        let ch = WeChatChannel::new(
            vec!["*".into()],
            None,
            None,
            Some("/tmp/test-wechat".into()),
        );
        assert!(ch.is_user_allowed("anyone@im.wechat"));
    }

    #[test]
    fn is_user_allowed_specific() {
        let ch = WeChatChannel::new(
            vec!["user1@im.wechat".into()],
            None,
            None,
            Some("/tmp/test-wechat".into()),
        );
        assert!(ch.is_user_allowed("user1@im.wechat"));
        assert!(!ch.is_user_allowed("user2@im.wechat"));
    }

    #[test]
    fn random_wechat_uin_is_base64() {
        let uin = random_wechat_uin();
        assert!(!uin.is_empty());
        // Should be valid base64
        assert!(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &uin).is_ok());
    }

    #[test]
    fn extract_text_with_ref_msg() {
        let items = vec![serde_json::json!({
            "type": 1,
            "text_item": { "text": "reply text" },
            "ref_msg": { "title": "original message" }
        })];
        assert_eq!(
            extract_text_from_items(&items),
            "[引用: original message]\nreply text"
        );
    }

    #[test]
    fn parse_attachment_markers_extracts_multiple_types() {
        let message = "See this\n[IMAGE:/tmp/a.png]\n[DOCUMENT:https://example.com/a.pdf]";
        let (cleaned, attachments) = parse_attachment_markers(message);

        assert_eq!(cleaned, "See this");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, WeChatAttachmentKind::Image);
        assert_eq!(attachments[0].target, "/tmp/a.png");
        assert_eq!(attachments[1].kind, WeChatAttachmentKind::Document);
        assert_eq!(attachments[1].target, "https://example.com/a.pdf");
    }

    #[test]
    fn parse_attachment_markers_keeps_invalid_marker_text() {
        let message = "See [UNKNOWN:/tmp/a.bin]";
        let (cleaned, attachments) = parse_attachment_markers(message);
        assert_eq!(cleaned, message);
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_path_only_attachment_detects_existing_file() {
        let temp = tempdir().unwrap();
        let image_path = temp.path().join("photo.png");
        std::fs::write(&image_path, b"png").unwrap();

        let parsed = parse_path_only_attachment(image_path.to_string_lossy().as_ref())
            .expect("expected attachment");
        assert_eq!(parsed.kind, WeChatAttachmentKind::Image);
        assert_eq!(parsed.target, image_path.to_string_lossy());
    }

    #[test]
    fn parse_path_only_attachment_rejects_sentence_text() {
        assert!(parse_path_only_attachment("saved to /tmp/photo.png").is_none());
    }

    #[test]
    fn format_attachment_content_uses_image_marker_for_images() {
        let path = PathBuf::from("/tmp/workspace/photo.png");
        assert_eq!(
            format_attachment_content("photo.png", &path),
            "[IMAGE:/tmp/workspace/photo.png]"
        );
    }

    #[test]
    fn format_attachment_content_uses_document_marker_for_non_images() {
        let path = PathBuf::from("/tmp/workspace/report.pdf");
        assert_eq!(
            format_attachment_content("report.pdf", &path),
            "[Document: report.pdf] /tmp/workspace/report.pdf"
        );
    }

    #[test]
    fn parse_aes_key_accepts_hex_and_base64() {
        let raw = *b"0123456789abcdef";
        let hex_key = hex::encode(raw);
        let base64_key = base64::engine::general_purpose::STANDARD.encode(raw);

        assert_eq!(parse_aes_key(&hex_key).unwrap(), raw);
        assert_eq!(parse_aes_key(&base64_key).unwrap(), raw);
    }

    #[test]
    fn find_inbound_attachment_prefers_direct_media() {
        let items = vec![
            serde_json::json!({
                "type": 1,
                "text_item": { "text": "caption" },
                "ref_msg": {
                    "message_item": {
                        "type": 4,
                        "file_item": {
                            "media": {
                                "encrypt_query_param": "quoted"
                            },
                            "file_name": "quoted.pdf"
                        }
                    }
                }
            }),
            serde_json::json!({
                "type": 2,
                "image_item": {
                    "media": {
                        "encrypt_query_param": "direct"
                    }
                }
            }),
        ];

        let spec = WeChatChannel::find_inbound_attachment(&items, "123").unwrap();
        assert_eq!(spec._kind, WeChatAttachmentKind::Image);
        assert_eq!(spec.encrypted_query_param, "direct");
    }

    #[test]
    fn markdown_to_plain_text_strips_common_formatting() {
        let input = "# Title\n**bold** [link](https://example.com)\n\n```rust\nlet x = 1;\n```";
        assert_eq!(
            markdown_to_plain_text(input),
            "Title\nbold link\n\nlet x = 1;"
        );
    }

    #[test]
    fn build_base_info_includes_channel_version() {
        let base_info = build_base_info();
        let version = base_info
            .get("channel_version")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        assert!(!version.is_empty());
    }
}
