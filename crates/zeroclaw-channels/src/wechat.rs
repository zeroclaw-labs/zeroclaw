//! WeChat personal iLink Bot channel.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::paths::{normalize_lexical, resolve_under};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::i18n;
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

/// State-dir file holding the persisted bot token / account identity.
/// Single source of truth for every reader, writer, and the relink purge.
const ACCOUNT_FILE: &str = "account.json";
/// State-dir file holding the persisted sync cursor and context tokens.
const SYNC_FILE: &str = "sync.json";

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

fn wechat_cli_string(key: &str) -> String {
    i18n::get_required_cli_string(key)
}

fn wechat_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    i18n::get_required_cli_string_with_args(key, args)
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
    kind: WeChatAttachmentKind,
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

fn is_remote_url(target: &str) -> bool {
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

    if !is_remote_url(candidate) && !Path::new(candidate).exists() {
        return None;
    }

    Some(WeChatAttachment {
        kind,
        target: candidate.to_string(),
    })
}

fn format_attachment_content(
    kind: WeChatAttachmentKind,
    local_filename: &str,
    local_path: &Path,
) -> String {
    if kind == WeChatAttachmentKind::Image {
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
        .map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "media encrypt failed"
            );
            anyhow::Error::msg(format!("media encrypt failed: {e}"))
        })?;
    Ok(encrypted.to_vec())
}

fn decrypt_aes_ecb(ciphertext: &[u8], key: &[u8; 16]) -> anyhow::Result<Vec<u8>> {
    let mut buffer = ciphertext.to_vec();
    Aes128EcbDec::new(&(*key).into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map(|decrypted| decrypted.to_vec())
        .map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "wechat: media decrypt failed"
            );
            anyhow::Error::msg(format!("media decrypt failed: {e}"))
        })
}

fn parse_aes_key(raw: &str) -> anyhow::Result<[u8; 16]> {
    let raw = raw.trim();
    if raw.len() == 32 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = hex::decode(raw).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "media hex aes_key invalid"
            );
            anyhow::Error::msg(format!("media hex aes_key invalid: {e}"))
        })?;
        return <[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"key_kind": "hex", "expected_bytes": 16})),
                "wechat: media hex aes_key has wrong byte length"
            );
            anyhow::Error::msg("media hex aes_key must be 16 bytes")
        });
    }

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "media base64 aes_key invalid"
            );
            anyhow::Error::msg(format!("media base64 aes_key invalid: {e}"))
        })?;

    if decoded.len() == 16 {
        return <[u8; 16]>::try_from(decoded.as_slice()).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"key_kind": "base64", "expected_bytes": 16})),
                "wechat: media base64 aes_key has wrong byte length"
            );
            anyhow::Error::msg("media base64 aes_key must be 16 bytes")
        });
    }

    if decoded.len() == 32 && decoded.iter().all(u8::is_ascii_hexdigit) {
        let hex_text = std::str::from_utf8(&decoded).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "media aes_key utf8 invalid"
            );
            anyhow::Error::msg(format!("media aes_key utf8 invalid: {e}"))
        })?;
        let bytes = hex::decode(hex_text).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "media nested hex aes_key invalid"
            );
            anyhow::Error::msg(format!("media nested hex aes_key invalid: {e}"))
        })?;
        return <[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(
                        ::serde_json::json!({"key_kind": "nested_hex", "expected_bytes": 16})
                    ),
                "wechat: media nested hex aes_key has wrong byte length"
            );
            anyhow::Error::msg("media nested hex aes_key must be 16 bytes")
        });
    }

    anyhow::bail!(
        "media aes_key must decode to 16 raw bytes or 32 hex chars, got {} bytes",
        decoded.len()
    )
}

fn https_base_url(
    field_name: &str,
    value: Option<String>,
    default: &str,
) -> anyhow::Result<String> {
    let url = value.unwrap_or_else(|| default.to_string());
    let url = url.trim().trim_end_matches('/').to_string();
    if !url.starts_with("https://") {
        anyhow::bail!("{field_name} must use https://, got {url}");
    }
    Ok(url)
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
    /// The alias key under `[channels.wechat.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    persist: Option<Arc<parking_lot::RwLock<Config>>>,
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

/// Persistent sync cursor and context tokens.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SyncData {
    #[serde(default)]
    get_updates_buf: String,
    #[serde(default)]
    context_tokens: HashMap<String, String>,
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

static CODE_BLOCK_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"```[^\n]*\n?([\s\S]*?)```").unwrap());
static IMAGE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap());
static LINK_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap());
static HEADING_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^\s{0,3}#{1,6}\s+").unwrap());
static BLOCKQUOTE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^>\s?").unwrap());
static BULLET_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^\s*[-*+]\s+").unwrap());
static EMPHASIS_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\*\*|__|~~|`|\*)").unwrap());
static TABLE_SEPARATOR_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\|[\s:|-]+\|$").unwrap());
static TABLE_ROW_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\|(.+)\|$").unwrap());

fn markdown_to_plain_text(text: &str) -> String {
    let mut result = CODE_BLOCK_RE.replace_all(text, "$1").into_owned();
    result = IMAGE_RE.replace_all(&result, "").into_owned();
    result = LINK_RE.replace_all(&result, "$1").into_owned();

    let mut lines = Vec::new();
    for line in result.lines() {
        if TABLE_SEPARATOR_RE.is_match(line) {
            continue;
        }

        if let Some(captures) = TABLE_ROW_RE.captures(line) {
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
    result = HEADING_RE.replace_all(&result, "").into_owned();
    result = BLOCKQUOTE_RE.replace_all(&result, "").into_owned();
    result = BULLET_RE.replace_all(&result, "").into_owned();
    result = EMPHASIS_RE.replace_all(&result, "").into_owned();

    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

fn render_login_qr(code: &str) -> anyhow::Result<String> {
    let payload = code.trim();
    if payload.is_empty() {
        anyhow::bail!("QR payload is empty");
    }

    let qr = qrcode::QrCode::new(payload.as_bytes()).map_err(|err| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
            "Failed to encode WeChat QR payload"
        );
        anyhow::Error::msg(format!("Failed to encode WeChat QR payload: {err}"))
    })?;

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
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        api_base_url: Option<String>,
        cdn_base_url: Option<String>,
        state_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let api_base_url = https_base_url("api_base_url", api_base_url, DEFAULT_API_BASE_URL)?;
        let cdn_base_url = https_base_url("cdn_base_url", cdn_base_url, CDN_BASE_URL)?;

        let alias = alias.into();
        let has_peers = !peer_resolver().is_empty();
        let pairing = if has_peers {
            None
        } else {
            let guard = PairingGuard::new(true, &[]);
            if let Some(code) = guard.pairing_code() {
                // Mirror Telegram: a backgrounded daemon discards stdout, so
                // also record the one-time bind code through the structured
                // log where `zeroclaw service logs` / the gateway can find it.
                // Tag it `Channel` so the web Logs page shows it by default
                // (an untagged event defaults to `Internal` and is hidden).
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_attrs(::serde_json::json!({
                            "alias": alias.as_str(),
                            "pairing_code": code.as_str(),
                        })),
                    "WeChat pairing required; one-time bind code issued"
                );
                println!(
                    "  {}",
                    wechat_cli_string_with_args("cli-wechat-pairing-required", &[("code", &code)],)
                );
                println!(
                    "     {}",
                    wechat_cli_string_with_args(
                        "cli-wechat-send-bind-command",
                        &[("command", WECHAT_BIND_COMMAND)],
                    )
                );
            }
            Some(guard)
        };

        let state_dir = state_dir.unwrap_or_else(Self::default_state_dir);

        let mut channel = Self {
            bot_token: RwLock::new(None),
            account_id: RwLock::new(None),
            api_base_url,
            cdn_base_url,
            alias,
            peer_resolver,
            persist: None,
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
        Ok(channel)
    }

    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Wire the shared Config handle so `persist_allowed_identity` can
    /// write a paired user into `peer_groups` and save. The long-running
    /// daemon sets this from the orchestrator; tests and one-shot
    /// callers leave it unset (pairing works at runtime, doesn't persist).
    pub fn with_persistence(mut self, config: Arc<parking_lot::RwLock<Config>>) -> Self {
        self.persist = Some(config);
        self
    }

    /// Default state directory when `[channels.wechat.<alias>] state_dir`
    /// is unset: `~/.zeroclaw/wechat`.
    fn default_state_dir() -> PathBuf {
        directories::UserDirs::new()
            .map(|u| u.home_dir().join(".zeroclaw").join("wechat"))
            .unwrap_or_else(|| PathBuf::from(".zeroclaw/wechat"))
    }

    /// Resolve the effective state directory from the raw
    /// `[channels.wechat.<alias>] state_dir` config value: tilde-expanded
    /// when set, [`Self::default_state_dir`] otherwise. Single source of
    /// truth for every consumer of the config value — channel construction
    /// and the readiness probe must agree on the directory.
    pub fn resolve_state_dir(configured: Option<&str>) -> PathBuf {
        match configured {
            Some(path) => PathBuf::from(shellexpand::tilde(path).as_ref()),
            None => Self::default_state_dir(),
        }
    }

    /// Read `account.json` from a state directory, if present and parseable.
    fn read_account_data(state_dir: &Path) -> Option<AccountData> {
        let data = std::fs::read_to_string(state_dir.join(ACCOUNT_FILE)).ok()?;
        serde_json::from_str::<AccountData>(&data).ok()
    }

    /// Channel-owned persisted-login probe: reports whether this state
    /// directory holds the same signal [`Self::load_persisted_state`] uses
    /// to resume a session without a fresh QR scan — an `account.json`
    /// carrying a non-empty bot token. Read-only; never creates files.
    pub fn has_persisted_login(state_dir: &Path) -> bool {
        Self::read_account_data(state_dir)
            .and_then(|account| account.token)
            .is_some_and(|token| !token.is_empty())
    }

    /// Channel-owned relink hook: delete the persisted login state so the
    /// next channel start finds no session and begins a fresh QR pairing.
    ///
    /// Removes exactly the files this module persists — [`ACCOUNT_FILE`]
    /// (the bot token, i.e. the credential) and [`SYNC_FILE`] (the sync
    /// cursor, which belongs to the replaced session) — and never the
    /// directory itself. Returns the paths actually removed; an already
    /// absent file is not an error, so relinking an unpaired channel is a
    /// safe no-op that returns an empty list.
    ///
    /// This only clears disk state. A currently running channel keeps its
    /// in-memory token until it is restarted; callers own scheduling that
    /// restart (e.g. a daemon reload).
    pub fn clear_persisted_login(state_dir: &Path) -> std::io::Result<Vec<String>> {
        let mut removed = Vec::new();
        for file in [ACCOUNT_FILE, SYNC_FILE] {
            let path = state_dir.join(file);
            match std::fs::remove_file(&path) {
                Ok(()) => removed.push(path.display().to_string()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(removed)
    }

    /// Load persisted token and cursor from state_dir.
    fn load_persisted_state(&mut self) {
        if let Some(account) = Self::read_account_data(&self.state_dir) {
            if let Some(ref token) = account.token
                && !token.is_empty()
            {
                *self.bot_token.write().unwrap() = Some(token.clone());
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "loaded persisted bot token"
                );
            }
            if let Some(ref id) = account.account_id {
                *self.account_id.write().unwrap() = Some(id.clone());
            }
        }

        let sync_path = self.state_dir.join(SYNC_FILE);
        if let Ok(data) = std::fs::read_to_string(&sync_path)
            && let Ok(sync) = serde_json::from_str::<SyncData>(&data)
        {
            if !sync.get_updates_buf.is_empty() {
                *self.cursor.lock() = sync.get_updates_buf;
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "loaded persisted sync cursor"
                );
            }
            if !sync.context_tokens.is_empty() {
                *self.context_tokens.lock() = sync.context_tokens;
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "loaded persisted context tokens"
                );
            }
        }
    }

    /// Save account data to disk.
    fn save_account_data(&self, token: &str, account_id: &str, user_id: Option<&str>) {
        if let Err(e) = std::fs::create_dir_all(&self.state_dir) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "failed to create state dir"
            );
            return;
        }
        let data = AccountData {
            token: Some(token.to_string()),
            account_id: Some(account_id.to_string()),
            base_url: Some(self.api_base_url.clone()),
            user_id: user_id.map(String::from),
            saved_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        let path = self.state_dir.join(ACCOUNT_FILE);
        match serde_json::to_string_pretty(&data) {
            Ok(json) => {
                if let Err(e) = write_private(&path, json.as_bytes()) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "failed to write account data"
                    );
                }
            }
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "failed to serialize account data"
            ),
        }
    }

    /// Save sync cursor to disk.
    fn save_sync_data(&self) {
        if let Err(e) = std::fs::create_dir_all(&self.state_dir) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "failed to create state dir"
            );
            return;
        }
        let data = SyncData {
            get_updates_buf: self.cursor.lock().clone(),
            context_tokens: self.context_tokens.lock().clone(),
        };
        let path = self.state_dir.join(SYNC_FILE);
        match serde_json::to_string(&data) {
            Ok(json) => {
                if let Err(e) = write_private(&path, json.as_bytes()) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "failed to write sync data"
                    );
                }
            }
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "failed to serialize sync data"
            ),
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
        self.save_sync_data();
    }

    fn get_context_token(&self, user_id: &str) -> Option<String> {
        self.context_tokens.lock().get(user_id).cloned()
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
    }

    async fn persist_allowed_identity(&self, identity: &str) -> anyhow::Result<()> {
        crate::identity_persist::persist_external_peer(
            self.persist.as_ref(),
            "wechat",
            &self.alias,
            identity,
        )
        .await
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

    fn canonicalize_within_workspace(
        candidate: &Path,
        workspace_dir: &Path,
        raw_target: &str,
    ) -> anyhow::Result<PathBuf> {
        let Ok(candidate_canon) = std::fs::canonicalize(candidate) else {
            return Ok(candidate.to_path_buf());
        };
        let workspace_canon = std::fs::canonicalize(workspace_dir).with_context(|| {
            format!(
                "workspace_dir {} could not be canonicalized",
                workspace_dir.display()
            )
        })?;
        if !candidate_canon.starts_with(&workspace_canon) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "attachment path {} canonicalizes to {} which escapes workspace {}",
                    raw_target,
                    candidate_canon.display(),
                    workspace_canon.display(),
                )
            );
            anyhow::bail!(
                "attachment path {} canonicalizes to {} which escapes workspace {}",
                raw_target,
                candidate_canon.display(),
                workspace_canon.display(),
            );
        }
        Ok(candidate_canon)
    }

    fn resolve_local_attachment_path(&self, target: &str) -> anyhow::Result<PathBuf> {
        let workspace_dir = self.workspace_dir.as_deref().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "workspace directory is not configured; cannot resolve local attachment path"
            );
            anyhow::Error::msg(
                "workspace directory is not configured; cannot resolve local attachment path",
            )
        })?;

        let target = target.trim();
        let target = target.strip_prefix("file://").unwrap_or(target);

        let workspace_normalized = normalize_lexical(workspace_dir);

        // `/workspace/...` is interpreted as relative to the workspace root.
        if let Some(rel) = target.strip_prefix("/workspace/") {
            let resolved = resolve_under(workspace_dir, rel).with_context(|| {
                format!(
                    "attachment path {} escapes workspace {}",
                    target,
                    workspace_dir.display()
                )
            })?;
            return Self::canonicalize_within_workspace(&resolved, workspace_dir, target);
        }

        // Absolute paths are allowed only if they are already inside the workspace.
        let candidate = Path::new(target);
        if candidate.is_absolute() {
            let normalized = normalize_lexical(candidate);
            if !normalized.starts_with(&workspace_normalized) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "attachment path {} escapes workspace {}, rejected",
                        target,
                        workspace_dir.display()
                    )
                );
                anyhow::bail!(
                    "attachment path {} escapes workspace {}",
                    target,
                    workspace_dir.display()
                );
            }
            return Self::canonicalize_within_workspace(&normalized, workspace_dir, target);
        }

        // Relative paths are resolved under the workspace root.
        let resolved = resolve_under(workspace_dir, target).with_context(|| {
            format!(
                "attachment path {} escapes workspace {}",
                target,
                workspace_dir.display()
            )
        })?;
        Self::canonicalize_within_workspace(&resolved, workspace_dir, target)
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
            anyhow::bail!("refusing non-HTTPS attachment URL: {url}");
        }
        let resp = self
            .client
            .get(url)
            .timeout(API_TIMEOUT)
            .send()
            .await
            .with_context(|| format!("attachment download failed: {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("attachment download failed ({status}): {body}");
        }

        if let Some(len) = resp.content_length()
            && len > WECHAT_MEDIA_MAX_BYTES
        {
            anyhow::bail!(
                "attachment Content-Length ({len} bytes) exceeds {} MB limit",
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
                "attachment exceeds {} MB limit",
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
        if is_remote_url(target) {
            return self
                .download_remote_attachment(target, attachment.kind)
                .await;
        }

        let path = self.resolve_local_attachment_path(target)?;
        if !path.exists() {
            anyhow::bail!("attachment path not found: {}", path.display());
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
            .with_context(|| format!("attachment read failed: {}", path.display()))?;
        if bytes.len() as u64 > WECHAT_MEDIA_MAX_BYTES {
            anyhow::bail!(
                "attachment exceeds {} MB limit",
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
            .context("not logged in, cannot upload attachment")?;
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
            anyhow::bail!("getUploadUrl failed ({status}): {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        data.get("upload_param")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .context("getUploadUrl returned no upload_param")
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
                        .context("CDN upload missing x-encrypted-param header")?;
                    return Ok(encrypted_param);
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "attempt": attempt,
                                "status": status.as_u16(),
                                "body": body,
                                "phase": "cdn_upload",
                            })),
                        "wechat: CDN upload failed (non-success status)"
                    );
                    let error = anyhow::Error::msg(format!(
                        "CDN upload failed on attempt {attempt} ({status}): {body}"
                    ));
                    if status.is_client_error() {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
                Err(err) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "attempt": attempt,
                                "phase": "cdn_upload",
                                "error": format!("{}", err),
                            })),
                        "wechat: CDN upload request failed"
                    );
                    last_error = Some(anyhow::Error::msg(format!(
                        "CDN upload request failed on attempt {attempt}: {err}"
                    )));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"phase": "cdn_upload"})),
                "wechat: CDN upload exhausted retries"
            );
            anyhow::Error::msg("CDN upload failed")
        }))
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

        // CDNMedia `aes_key` must be base64(hex(key)).
        // WeChat client base64-decodes then hex-decodes to recover the 16 bytes.
        let aes_key_base64 = base64::engine::general_purpose::STANDARD.encode(hex::encode(aes_key));

        Ok(UploadedWeChatMedia {
            encrypted_query_param,
            aes_key_base64,
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
                        kind: WeChatAttachmentKind::Image,
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
                        kind: WeChatAttachmentKind::Document,
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
                        kind: WeChatAttachmentKind::Video,
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
                        kind: WeChatAttachmentKind::Voice,
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
            anyhow::bail!("attachment download failed ({status}): {body}");
        }

        let bytes = resp.bytes().await?.to_vec();
        if bytes.len() as u64 > WECHAT_MEDIA_MAX_BYTES {
            anyhow::bail!(
                "inbound attachment exceeds {} MB limit",
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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    "attachment download skipped"
                );
                return None;
            }
        };

        let save_dir = workspace_dir.join("wechat_files");
        if let Err(err) = tokio::fs::create_dir_all(&save_dir).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "Failed to create WeChat attachment dir"
            );
            return None;
        }

        let local_path = save_dir.join(&spec.file_name);
        if let Err(err) = tokio::fs::write(&local_path, bytes).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Failed to save WeChat attachment to {}: {err}",
                    local_path.display()
                )
            );
            return None;
        }

        Some(format_attachment_content(
            spec.kind,
            &spec.file_name,
            &local_path,
        ))
    }

    /// Perform QR-code login flow. Returns (bot_token, account_id, user_id).
    async fn qr_login(&self) -> anyhow::Result<(String, String, Option<String>)> {
        let mut qr_refresh_count = 0u32;

        loop {
            qr_refresh_count += 1;
            if qr_refresh_count > MAX_QR_REFRESH {
                let max = MAX_QR_REFRESH.to_string();
                let reason = wechat_cli_string_with_args(
                    "cli-wechat-qr-expired-giving-up",
                    &[("max", &max)],
                );
                crate::login_events::LoginEvent::Failed { reason: &reason }.emit(
                    self.name(),
                    &self.alias,
                    "WeChat QR login gave up after repeated expiry",
                );
                anyhow::bail!("{reason}");
            }

            // Fetch QR code
            let qr_url = format!("{}?bot_type=3", self.api_url("get_bot_qrcode"));
            let resp = self
                .client
                .get(&qr_url)
                .timeout(API_TIMEOUT)
                .send()
                .await
                .with_context(|| wechat_cli_string("cli-wechat-qr-fetch-failed"))?;

            if !resp.status().is_success() {
                let status = resp.status().to_string();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!(
                    "{}",
                    wechat_cli_string_with_args(
                        "cli-wechat-qr-fetch-status-failed",
                        &[("status", &status), ("body", &body)],
                    )
                );
            }

            let qr_data: serde_json::Value = resp.json().await?;
            let qrcode = qr_data
                .get("qrcode")
                .and_then(|v| v.as_str())
                .with_context(|| {
                    wechat_cli_string_with_args(
                        "cli-wechat-missing-response-field",
                        &[("field", "qrcode")],
                    )
                })?
                .to_string();
            let qrcode_img_url = qr_data
                .get("qrcode_img_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Display QR code
            let qr_attempt = qr_refresh_count.to_string();
            let qr_max = MAX_QR_REFRESH.to_string();
            println!(
                "\n  {}",
                wechat_cli_string_with_args(
                    "cli-wechat-qr-login",
                    &[("attempt", &qr_attempt), ("max", &qr_max)],
                )
            );
            println!("  {}\n", wechat_cli_string("cli-wechat-scan-to-connect"));
            let qr_payload = if qrcode_img_url.is_empty() {
                qrcode.as_str()
            } else {
                qrcode_img_url
            };
            crate::login_events::LoginEvent::Qr {
                payload: qr_payload,
                image_url: (!qrcode_img_url.is_empty()).then_some(qrcode_img_url),
                attempt: Some(qr_refresh_count),
                max_attempts: Some(MAX_QR_REFRESH),
            }
            .emit(
                self.name(),
                &self.alias,
                "WeChat login QR code ready (scan with the WeChat app)",
            );
            match render_login_qr(qr_payload) {
                Ok(qr) => println!("{qr}"),
                Err(err) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                        "failed to render terminal QR code"
                    )
                }
            }
            if !qrcode_img_url.is_empty() {
                println!(
                    "  {}",
                    wechat_cli_string_with_args("cli-wechat-qr-url", &[("url", qrcode_img_url)],)
                );
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
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "QR poll error"
                        );
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
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "QR poll parse error"
                        );
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
                            println!("  {}", wechat_cli_string("cli-wechat-scanned-confirm"));
                            crate::login_events::LoginEvent::Scanned.emit(
                                self.name(),
                                &self.alias,
                                "WeChat QR code scanned — waiting for in-app confirmation",
                            );
                            scanned_printed = true;
                        }
                    }
                    "expired" => {
                        println!(
                            "  {}",
                            wechat_cli_string("cli-wechat-qr-expired-refreshing")
                        );
                        crate::login_events::LoginEvent::Expired {
                            attempt: qr_refresh_count,
                            max_attempts: MAX_QR_REFRESH,
                        }
                        .emit(
                            self.name(),
                            &self.alias,
                            "WeChat login QR code expired",
                        );
                        break; // Will loop back and get a new QR code
                    }
                    "confirmed" => {
                        let bot_token = status
                            .get("bot_token")
                            .and_then(|v| v.as_str())
                            .with_context(|| {
                                wechat_cli_string_with_args(
                                    "cli-wechat-login-confirmed-missing-field",
                                    &[("field", "bot_token")],
                                )
                            })?
                            .to_string();
                        let account_id = status
                            .get("ilink_bot_id")
                            .and_then(|v| v.as_str())
                            .with_context(|| {
                                wechat_cli_string_with_args(
                                    "cli-wechat-login-confirmed-missing-field",
                                    &[("field", "ilink_bot_id")],
                                )
                            })?
                            .to_string();
                        let user_id = status
                            .get("ilink_user_id")
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        println!("  {}", wechat_cli_string("cli-wechat-connected"));
                        crate::login_events::LoginEvent::Connected.emit(
                            self.name(),
                            &self.alias,
                            "WeChat login confirmed — channel connected",
                        );
                        return Ok((bot_token, account_id, user_id));
                    }
                    other => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"other": other})),
                            "QR status"
                        );
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

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "no persisted token, starting QR login..."
        );
        let (token, account_id, user_id) = self.qr_login().await?;

        // Save to memory
        if let Ok(mut t) = self.bot_token.write() {
            *t = Some(token.clone());
        }
        if let Ok(mut a) = self.account_id.write() {
            *a = Some(account_id.clone());
        }

        // If a user scanned, persist them as an allowed peer
        if let Some(ref uid) = user_id
            && let Err(e) = self.persist_allowed_identity(uid).await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e), "uid": uid})),
                "failed to persist scanned identity"
            );
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
        let token = self.get_token().context("not logged in, cannot send")?;

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
            anyhow::bail!("sendMessage failed ({status}): {err}");
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
                        if let Err(e) = self.persist_allowed_identity(from_user_id).await {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"from_user_id": from_user_id, "e": e.to_string()})), "failed to persist bound identity");
                        }
                        let ctx = self.get_context_token(from_user_id);
                        let reply = wechat_cli_string("cli-wechat-bound-success");
                        let _ = self.send_text(from_user_id, &reply, ctx.as_deref()).await;
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"from_user_id": from_user_id})),
                            "user bound via pairing code"
                        );
                    }
                    Ok(None) => {
                        let ctx = self.get_context_token(from_user_id);
                        let reply = wechat_cli_string("cli-wechat-invalid-bind-code");
                        let _ = self.send_text(from_user_id, &reply, ctx.as_deref()).await;
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "pairing error"
                        );
                    }
                }
            }
        } else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"from_user_id": from_user_id})),
                "ignoring unauthorized message from"
            );
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for WeChatChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Wechat)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for WeChatChannel {
    fn name(&self) -> &str {
        "wechat"
    }

    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let recipient = &message.recipient;
        let content = crate::util::strip_tool_call_tags(&message.content);
        let context_token = self.get_context_token(recipient);

        if context_token.is_none() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"recipient": recipient})),
                "no context_token for , message may fail to associate"
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

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "channel listening for messages..."
        );

        let mut cursor = self.cursor.lock().clone();
        let mut long_poll_timeout_ms = LONG_POLL_TIMEOUT_MS;
        let mut consecutive_failures: u32 = 0;

        loop {
            let token = match self.get_token() {
                Some(t) => t,
                None => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "token lost, attempting re-login..."
                    );
                    if let Err(e) = self.ensure_logged_in().await {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "re-login failed"
                        );
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
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"consecutive_failures": consecutive_failures, "MAX_CONSECUTIVE_FAILURES": MAX_CONSECUTIVE_FAILURES, "e": e.to_string()})), "getUpdates error (/)");
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
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "getUpdates: client-side timeout, retrying"
                    );
                    continue;
                }
            };

            let data: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    consecutive_failures += 1;
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "getUpdates parse error"
                    );
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
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        &format!(
                            "session expired (errcode {SESSION_EXPIRED_ERRCODE}), pausing for {} min",
                            SESSION_PAUSE_DURATION.as_secs() / 60
                        )
                    );
                    // Clear token so we re-login after pause
                    if let Ok(mut t) = self.bot_token.write() {
                        *t = None;
                    }
                    self.context_tokens.lock().clear();
                    self.save_sync_data();
                    tokio::time::sleep(SESSION_PAUSE_DURATION).await;
                    // Try to re-login
                    if let Err(e) = self.ensure_logged_in().await {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "re-login after session expiry failed"
                        );
                    }
                    consecutive_failures = 0;
                    continue;
                }

                consecutive_failures += 1;
                let errmsg = data.get("errmsg").and_then(|v| v.as_str()).unwrap_or("");
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"ret": ret, "errcode": errcode, "errmsg": errmsg, "consecutive_failures": consecutive_failures, "MAX_CONSECUTIVE_FAILURES": MAX_CONSECUTIVE_FAILURES})), "getUpdates failed: ret= errcode= errmsg= (/)");
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    consecutive_failures = 0;
                    tokio::time::sleep(BACKOFF_DELAY).await;
                } else {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                continue;
            }

            consecutive_failures = 0;

            // Capture the response cursor but defer committing it (both the
            // local `cursor` and `self.cursor`/disk) until every message in
            // this batch has been successfully enqueued below. See the
            // commit site after the `for msg in &msgs` loop for why.
            let next_cursor = data
                .get("get_updates_buf")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

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
                    channel_alias: Some(self.alias.clone()),
                    timestamp,
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: Vec::new(),
                    subject: None,

                    ..Default::default()
                };

                if tx.send(channel_msg).await.is_err() {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "channel receiver dropped, stopping"
                    );
                    // Do NOT commit `next_cursor` here: the batch is only
                    // partially (or not at all) enqueued, so the old
                    // cursor must stay on disk. On supervised restart
                    // `listen()` reloads it and re-polls this batch.
                    return Ok(());
                }
            }

            // Commit the cursor only now that the whole batch has been
            // enqueued (or there was nothing to enqueue). Persisting any
            // earlier — e.g. right after reading the getupdates response,
            // before this loop — would let a crash/exit between cursor
            // persistence and enqueue completion permanently lose the
            // batch: on restart, `listen()` would reload the
            // already-advanced cursor and never re-poll those messages.
            // There is no inbound dedup, so redelivery after a restart is
            // possible (at-least-once); that trade is intentional and
            // preferable to silent message loss.
            if let Some(new_cursor) = next_cursor {
                cursor = new_cursor;
                *self.cursor.lock() = cursor.clone();
                self.save_sync_data();
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

        let handle = zeroclaw_spawn::spawn!(async move {
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

    async fn send_draft(&self, _msg: &SendMessage) -> anyhow::Result<Option<String>> {
        // TODO: Re-enable placeholder if WeChat adds message edit/revoke support.
        // Current behavior: Return draft_id without sending placeholder.
        // The final response will be sent in finalize_draft().
        let draft_id = format!("draft_{}", uuid::Uuid::new_v4());
        Ok(Some(draft_id))
    }

    async fn update_draft(
        &self,
        _recipient: &str,
        _draft_id: &str,
        _content: &str,
    ) -> anyhow::Result<()> {
        // WeChat iLink doesn't support message editing.
        // We accumulate deltas in the draft_updater task and only send the final
        // message in finalize_draft(). This method is a no-op.
        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        _draft_id: &str,
        content: &str,
        _suppress_voice: bool,
    ) -> anyhow::Result<()> {
        // Send the final accumulated response
        let result = self
            .send(&SendMessage::new(
                content.to_string(),
                recipient.to_string(),
            ))
            .await;
        let _ = self.stop_typing(recipient).await; // Always stop the typing indicator
        result
    }

    async fn cancel_draft(&self, recipient: &str, _draft_id: &str) -> anyhow::Result<()> {
        self.stop_typing(recipient).await
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        _draft_id: &str,
        _progress: &str,
    ) -> anyhow::Result<()> {
        // Use the typing indicator instead of message updates
        let _ = self.start_typing(recipient).await;
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
            "wechat_test_alias",
            Arc::new(|| vec!["*".into()]),
            None,
            None,
            Some("/tmp/test-wechat".into()),
        )
        .unwrap();
        assert_eq!(ch.name(), "wechat");
    }

    #[test]
    fn has_persisted_login_requires_non_empty_account_token() {
        let temp = tempdir().unwrap();
        let dir = temp.path();

        assert!(!WeChatChannel::has_persisted_login(dir));

        // A token cleared on logout is not a persisted login.
        std::fs::write(dir.join("account.json"), r#"{"token": ""}"#).unwrap();
        assert!(!WeChatChannel::has_persisted_login(dir));

        std::fs::write(
            dir.join("account.json"),
            r#"{"token": "tok_persisted", "account_id": "acct_1"}"#,
        )
        .unwrap();
        assert!(WeChatChannel::has_persisted_login(dir));
    }

    #[test]
    fn clear_persisted_login_removes_state_files_and_is_idempotent() {
        let temp = tempdir().unwrap();
        let dir = temp.path();
        std::fs::write(dir.join("account.json"), r#"{"token": "tok_persisted"}"#).unwrap();
        std::fs::write(dir.join("sync.json"), r#"{"get_updates_buf": "cursor"}"#).unwrap();

        let removed = WeChatChannel::clear_persisted_login(dir).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!dir.join("account.json").exists());
        assert!(!dir.join("sync.json").exists());
        assert!(!WeChatChannel::has_persisted_login(dir));
        assert!(dir.exists(), "the state directory itself must survive");

        // Relinking an already unpaired channel is a safe no-op.
        let removed = WeChatChannel::clear_persisted_login(dir).unwrap();
        assert!(removed.is_empty());
    }

    #[test]
    fn wechat_channel_rejects_http_api_base_url() {
        let result = WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(|| vec!["*".into()]),
            Some("http://ilink.example.test".into()),
            None,
            Some("/tmp/test-wechat".into()),
        );
        assert!(result.is_err());

        let err = result.err().unwrap();
        assert!(err.to_string().contains("api_base_url must use https://"));
    }

    #[test]
    fn wechat_channel_rejects_http_cdn_base_url() {
        let result = WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(|| vec!["*".into()]),
            None,
            Some("http://cdn.example.test".into()),
            Some("/tmp/test-wechat".into()),
        );
        assert!(result.is_err());

        let err = result.err().unwrap();
        assert!(err.to_string().contains("cdn_base_url must use https://"));
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
            "wechat_test_alias",
            Arc::new(|| vec!["*".into()]),
            None,
            None,
            Some("/tmp/test-wechat".into()),
        )
        .unwrap();
        assert!(ch.is_user_allowed("anyone@im.wechat"));
    }

    #[test]
    fn is_user_allowed_specific() {
        let ch = WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(|| vec!["user1@im.wechat".into()]),
            None,
            None,
            Some("/tmp/test-wechat".into()),
        )
        .unwrap();
        assert!(ch.is_user_allowed("user1@im.wechat"));
        assert!(!ch.is_user_allowed("user2@im.wechat"));
    }

    #[tokio::test]
    async fn persist_allowed_identity_without_handle_warns_and_returns_ok() {
        let ch = WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(Vec::new),
            None,
            None,
            Some("/tmp/test-wechat".into()),
        )
        .unwrap();
        // No `.with_persistence(...)` wired — should not panic, returns Ok(()).
        let result = ch.persist_allowed_identity("user_xyz@im.wechat").await;
        assert!(result.is_ok());
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
            format_attachment_content(WeChatAttachmentKind::Image, "photo.png", &path),
            "[IMAGE:/tmp/workspace/photo.png]"
        );
    }

    #[test]
    fn format_attachment_content_uses_document_marker_for_non_images() {
        let path = PathBuf::from("/tmp/workspace/report.pdf");
        assert_eq!(
            format_attachment_content(WeChatAttachmentKind::Document, "report.pdf", &path),
            "[Document: report.pdf] /tmp/workspace/report.pdf"
        );
    }

    fn test_wechat_channel_with_workspace(workspace_dir: &Path) -> WeChatChannel {
        WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(|| vec!["*".into()]),
            None,
            None,
            Some(workspace_dir.join("state")),
        )
        .unwrap()
        .with_workspace_dir(workspace_dir.to_path_buf())
    }

    #[test]
    fn resolve_local_attachment_path_requires_workspace_dir() {
        let temp = tempdir().unwrap();
        let ch = WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(|| vec!["*".into()]),
            None,
            None,
            Some(temp.path().join("state")),
        )
        .unwrap();
        let err = ch.resolve_local_attachment_path("photo.png").unwrap_err();
        assert!(
            err.to_string()
                .contains("workspace directory is not configured"),
            "got: {err}"
        );
    }

    #[test]
    fn resolve_local_attachment_path_accepts_relative_workspace_path() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        assert_eq!(
            ch.resolve_local_attachment_path("photo.png").unwrap(),
            workspace.join("photo.png")
        );
    }

    #[test]
    fn resolve_local_attachment_path_accepts_workspace_prefix() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        assert_eq!(
            ch.resolve_local_attachment_path("/workspace/photo.png")
                .unwrap(),
            workspace.join("photo.png")
        );
    }

    #[test]
    fn resolve_local_attachment_path_accepts_file_uri_with_workspace_prefix() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        assert_eq!(
            ch.resolve_local_attachment_path("file:///workspace/photo.png")
                .unwrap(),
            workspace.join("photo.png")
        );
    }

    #[test]
    fn resolve_local_attachment_path_accepts_absolute_path_inside_workspace() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        let file = workspace.join("photo.png");
        assert_eq!(
            ch.resolve_local_attachment_path(file.to_str().unwrap())
                .unwrap(),
            file
        );
    }

    #[test]
    fn resolve_local_attachment_path_normalizes_within_workspace() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        assert_eq!(
            ch.resolve_local_attachment_path("/workspace/sub/../photo.png")
                .unwrap(),
            workspace.join("photo.png")
        );
    }

    #[test]
    fn resolve_local_attachment_path_rejects_dotdot_escape() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        assert!(
            ch.resolve_local_attachment_path("/workspace/../etc/passwd")
                .is_err(),
            "dotdot escape with /workspace/ prefix should be rejected"
        );
        assert!(
            ch.resolve_local_attachment_path("sub/../../etc/passwd")
                .is_err(),
            "relative dotdot escape should be rejected"
        );
    }

    #[test]
    fn resolve_local_attachment_path_rejects_absolute_outside_workspace() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        assert!(
            ch.resolve_local_attachment_path("/etc/passwd").is_err(),
            "absolute path outside workspace should be rejected"
        );
        assert!(
            ch.resolve_local_attachment_path("file:///etc/passwd")
                .is_err(),
            "file URI outside workspace should be rejected"
        );
    }

    #[test]
    #[cfg(unix)] // `std::os::unix::fs::symlink` is Unix-only; on Windows the
    // lexical-only containment path is still exercised by the
    // other tests in this module.
    fn resolve_local_attachment_path_rejects_symlink_escaping_workspace() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let outside_dir = temp.path().join("outside-target");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let outside_file = outside_dir.join("secret.txt");
        std::fs::write(&outside_file, "top secret").unwrap();
        std::os::unix::fs::symlink(&outside_dir, workspace.join("outside")).unwrap();

        let ch = test_wechat_channel_with_workspace(&workspace);
        let err = ch
            .resolve_local_attachment_path("/workspace/outside/secret.txt")
            .expect_err("symlink that escapes workspace must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("canonicalizes to") && msg.contains("escapes workspace"),
            "expected canonical-escape error, got: {msg}"
        );
    }

    #[test]
    #[cfg(unix)] // Symlink creation is Unix-only; the test still proves the
    // canonical-containment path on the platforms where it runs.
    fn resolve_local_attachment_path_accepts_symlink_within_workspace() {
        // Workspace-internal symlinks are legitimate aliases (e.g. a
        // `latest -> 2026-07-03` link inside an attachments directory).
        // They must still resolve cleanly so the upload sees the real file.
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let real_dir = workspace.join("attachments").join("2026-07-03");
        std::fs::create_dir_all(&real_dir).unwrap();
        let real_file = real_dir.join("report.pdf");
        std::fs::write(&real_file, b"%PDF-1.4\n").unwrap();
        std::os::unix::fs::symlink(&real_dir, workspace.join("latest")).unwrap();

        let ch = test_wechat_channel_with_workspace(&workspace);
        let resolved = ch
            .resolve_local_attachment_path("/workspace/latest/report.pdf")
            .expect("workspace-internal symlink alias must be accepted");
        let real_canon = std::fs::canonicalize(&real_file).unwrap();
        assert_eq!(resolved, real_canon);
    }

    #[test]
    fn resolve_local_attachment_path_allows_nonexistent_lexical_target() {
        // Non-existent paths must still pass (a future-write path, or a
        // target the agent has not created yet). The canonical-containment
        // check is skipped because canonicalize() would fail.
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        let resolved = ch
            .resolve_local_attachment_path("/workspace/not-yet-created.png")
            .expect("non-existent path under workspace is allowed (lexical only)");
        assert_eq!(resolved, workspace.join("not-yet-created.png"));
    }

    #[tokio::test]
    async fn load_attachment_payload_rejects_path_traversal() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let ch = test_wechat_channel_with_workspace(&workspace);
        let attachment = WeChatAttachment {
            kind: WeChatAttachmentKind::Image,
            target: "/workspace/../etc/passwd".to_string(),
        };
        let err = ch.load_attachment_payload(&attachment).await.unwrap_err();
        assert!(err.to_string().contains("escapes workspace"), "got: {err}");
    }

    #[test]
    fn parse_aes_key_accepts_hex_and_base64() {
        let raw: [u8; 16] = *b"0123456789abcdef";
        let hex_key = hex::encode(raw);
        let base64_key = base64::engine::general_purpose::STANDARD.encode(raw);

        // Inbound accepts plain hex and base64(raw bytes).
        assert_eq!(parse_aes_key(&hex_key).unwrap(), raw);
        assert_eq!(parse_aes_key(&base64_key).unwrap(), raw);

        let outbound = base64::engine::general_purpose::STANDARD.encode(hex::encode(raw));
        assert_ne!(outbound, base64_key);
        assert_eq!(parse_aes_key(&outbound).unwrap(), raw);
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
        assert_eq!(spec.kind, WeChatAttachmentKind::Image);
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

    #[test]
    fn sync_data_round_trip_preserves_context_tokens() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();

        let mut context_tokens = HashMap::new();
        context_tokens.insert("user123".to_string(), "token_abc".to_string());
        context_tokens.insert("user456".to_string(), "token_xyz".to_string());

        let original_data = SyncData {
            get_updates_buf: "cursor_value".to_string(),
            context_tokens: context_tokens.clone(),
        };

        let sync_path = state_dir.join("sync.json");
        let json = serde_json::to_string(&original_data).unwrap();
        write_private(&sync_path, json.as_bytes()).unwrap();

        let loaded_json = std::fs::read_to_string(&sync_path).unwrap();
        let loaded_data: SyncData = serde_json::from_str(&loaded_json).unwrap();

        assert_eq!(loaded_data.get_updates_buf, "cursor_value");
        assert_eq!(loaded_data.context_tokens.len(), 2);
        assert_eq!(
            loaded_data.context_tokens.get("user123"),
            Some(&"token_abc".to_string())
        );
        assert_eq!(
            loaded_data.context_tokens.get("user456"),
            Some(&"token_xyz".to_string())
        );
    }

    #[test]
    fn sync_data_backward_compatible_with_missing_context_tokens() {
        let old_json = r#"{"get_updates_buf":"old_cursor"}"#;
        let data: SyncData = serde_json::from_str(old_json).unwrap();

        assert_eq!(data.get_updates_buf, "old_cursor");
        assert!(data.context_tokens.is_empty());
    }

    #[test]
    fn context_tokens_survive_channel_restart() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();

        {
            let ch = WeChatChannel::new(
                "test",
                Arc::new(|| vec!["*".to_string()]),
                None,
                None,
                Some(state_dir.clone()),
            )
            .unwrap();
            ch.set_context_token("acct1:userA", "tok_A");
            ch.set_context_token("acct1:userB", "tok_B");
            *ch.cursor.lock() = "cursor_123".to_string();
            ch.save_sync_data();
        }

        let ch2 = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir),
        )
        .unwrap();

        assert_eq!(
            ch2.get_context_token("acct1:userA"),
            Some("tok_A".to_string())
        );
        assert_eq!(
            ch2.get_context_token("acct1:userB"),
            Some("tok_B".to_string())
        );
        assert_eq!(ch2.get_context_token("nonexistent"), None);
        assert_eq!(*ch2.cursor.lock(), "cursor_123");
    }

    #[test]
    fn set_context_token_persists_immediately() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();

        let ch = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        ch.set_context_token("acct:user1", "immediate_tok");

        let ch2 = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir),
        )
        .unwrap();
        assert_eq!(
            ch2.get_context_token("acct:user1"),
            Some("immediate_tok".to_string())
        );
    }

    #[test]
    fn save_sync_data_preserves_context_tokens() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();

        let ch = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        ch.set_context_token("acct:user1", "my_token");
        *ch.cursor.lock() = "new_cursor_value".to_string();
        ch.save_sync_data();

        let ch2 = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir),
        )
        .unwrap();
        assert_eq!(*ch2.cursor.lock(), "new_cursor_value");
        assert_eq!(
            ch2.get_context_token("acct:user1"),
            Some("my_token".to_string())
        );
    }

    #[test]
    fn load_from_empty_state_dir_produces_defaults() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();

        let ch = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir),
        )
        .unwrap();

        assert_eq!(ch.get_context_token("anything"), None);
        assert_eq!(*ch.cursor.lock(), "");
    }

    #[test]
    fn context_token_overwrite_persists_latest() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();

        let ch = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        ch.set_context_token("acct:user1", "old_token");
        ch.set_context_token("acct:user1", "new_token");

        let ch2 = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir),
        )
        .unwrap();
        assert_eq!(
            ch2.get_context_token("acct:user1"),
            Some("new_token".to_string())
        );
    }

    /// Build a `WeChatChannel` wired to a wiremock server. `WeChatChannel::new`
    /// rejects non-https `api_base_url` values (see
    /// `wechat_channel_rejects_http_api_base_url` above) and `MockServer::uri()`
    /// is `http://127.0.0.1:<port>`, so we construct with the (unused) https
    /// default and then overwrite the private `api_base_url` field directly —
    /// legal here because this test module is nested inside the same file and
    /// therefore shares its privacy scope with `WeChatChannel`.
    fn wechat_channel_for_mock(state_dir: PathBuf, mock_base_url: String) -> WeChatChannel {
        let mut ch = WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir),
        )
        .unwrap();
        ch.api_base_url = mock_base_url;
        *ch.bot_token.write().unwrap() = Some("test-token".to_string());
        ch
    }

    fn getupdates_batch(cursor: &str, msgs: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "ret": 0,
            "errcode": 0,
            "get_updates_buf": cursor,
            "msgs": msgs,
        })
    }

    /// Regression test for lost inbound batches: if the very first
    /// `tx.send` in a batch fails (receiver gone), `listen()` must return without ever
    /// committing the cursor the response carried — otherwise a crash
    /// between cursor persistence and enqueue completion would
    /// permanently skip the un-enqueued messages on restart.
    #[tokio::test]
    async fn listen_does_not_commit_cursor_when_first_enqueue_fails() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "item_list": [{"type": 1, "text_item": {"text": "hello"}}]
                    },
                    {
                        "from_user_id": "user_b",
                        "message_id": 2,
                        "create_time_ms": 1_700_000_001_000u64,
                        "item_list": [{"type": 1, "text_item": {"text": "world"}}]
                    }
                ]),
            )))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock(state_dir.clone(), mock_server.uri());
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx); // first tx.send in the batch will fail immediately

        let result = tokio::time::timeout(Duration::from_secs(5), ch.listen(tx))
            .await
            .expect("listen() should return promptly once the receiver is gone");
        assert!(result.is_ok());

        // Probe through the production reload path (`load_persisted_state`
        // via the constructor) — exactly what a supervised restart runs.
        let probe = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        assert_eq!(
            *probe.cursor.lock(),
            "original_cursor",
            "cursor must not advance when the batch was never enqueued"
        );
    }

    /// Happy path for the deferred cursor commit: once a batch is fully
    /// enqueued, its cursor commits. A second batch whose enqueue fails (receiver
    /// dropped mid-flight) must NOT move the cursor further.
    #[tokio::test]
    async fn listen_commits_cursor_only_after_batch_fully_enqueued() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();
        let mock_server = MockServer::start().await;

        // First batch: fully drained by the test below, so its cursor
        // must be committed.
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_batch_1",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "item_list": [{"type": 1, "text_item": {"text": "hello"}}]
                    },
                    {
                        "from_user_id": "user_b",
                        "message_id": 2,
                        "create_time_ms": 1_700_000_001_000u64,
                        "item_list": [{"type": 1, "text_item": {"text": "world"}}]
                    }
                ]),
            )))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second batch: the receiver is dropped as soon as batch 1 has
        // been fully drained, before this response's message is
        // enqueued, so this cursor must never be committed.
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_batch_2",
                serde_json::json!([
                    {
                        "from_user_id": "user_c",
                        "message_id": 3,
                        "create_time_ms": 1_700_000_002_000u64,
                        "item_list": [{"type": 1, "text_item": {"text": "third"}}]
                    }
                ]),
            )))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock(state_dir.clone(), mock_server.uri());
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for first message")
            .expect("channel closed before first message");
        assert_eq!(first.sender, "user_a");

        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for second message")
            .expect("channel closed before second message");
        assert_eq!(second.sender, "user_b");

        // Batch 1 is fully drained now (both sends returned Ok). Drop the
        // receiver synchronously, before yielding back to the executor, so
        // batch 2's send is guaranteed to observe a closed channel.
        drop(rx);

        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("listen() task timed out")
            .expect("listen() task panicked");
        assert!(result.is_ok());

        // Probe through the production reload path (`load_persisted_state`
        // via the constructor) — exactly what a supervised restart runs.
        let probe = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        assert_eq!(
            *probe.cursor.lock(),
            "cursor_batch_1",
            "cursor should advance to batch 1's cursor, not batch 2's"
        );
    }

    /// Covers the subtlety the fix hinges on: `set_context_token` (called
    /// mid-batch, for the first message) itself calls `save_sync_data()`.
    /// Because cursor commitment is deferred until the whole batch is
    /// enqueued, that mid-batch save must still see (and persist) the OLD
    /// cursor — even though the getupdates response already carried a new
    /// one — while still recording the new context token.
    #[tokio::test]
    async fn listen_mid_batch_context_token_save_keeps_old_cursor() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().to_path_buf();
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "context_token": "ctx_abc123",
                        "item_list": [{"type": 1, "text_item": {"text": "hello"}}]
                    },
                    {
                        "from_user_id": "user_b",
                        "message_id": 2,
                        "create_time_ms": 1_700_000_001_000u64,
                        "item_list": [{"type": 1, "text_item": {"text": "world"}}]
                    }
                ]),
            )))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock(state_dir.clone(), mock_server.uri());
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for first message")
            .expect("channel closed before first message");
        assert_eq!(first.sender, "user_a");

        // Drop synchronously (no intervening await) so message 2's send
        // observes a closed channel and the batch never completes.
        drop(rx);

        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("listen() task timed out")
            .expect("listen() task panicked");
        assert!(result.is_ok());

        // Probe through the production reload path (`load_persisted_state`
        // via the constructor) — exactly what a supervised restart runs.
        let probe = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        assert_eq!(
            probe.get_context_token("user_a"),
            Some("ctx_abc123".to_string()),
            "mid-batch set_context_token must still persist the new token"
        );
        assert_eq!(
            *probe.cursor.lock(),
            "original_cursor",
            "mid-batch save must not have leaked the uncommitted new cursor"
        );
    }
}
