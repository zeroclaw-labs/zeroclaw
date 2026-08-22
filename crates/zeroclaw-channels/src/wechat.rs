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
/// Initial delay before re-polling a batch held back by a retryable
/// attachment failure. Doubles per consecutive held pass, capped at
/// `ATTACHMENT_RETRY_MAX_DELAY`, and resets as soon as a batch commits.
/// Without this, a held batch re-polls immediately in a tight loop for
/// as long as the CDN keeps failing.
const ATTACHMENT_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
/// Ceiling for the attachment-retry backoff.
const ATTACHMENT_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);
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

/// Outcome of attempting to build inbound attachment content for a
/// message, distinguishing "nothing to do" from a failure, and a
/// retryable failure from a permanent one.
///
/// The listener uses this to decide whether it may commit `next_cursor`
/// for the batch: `Retryable` must hold the cursor so the message is
/// re-fetched (and the attachment retried) after a restart; `Permanent`
/// and `None` must not hold the cursor, since nothing further will
/// change on retry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AttachmentDisposition {
    /// The message carries no fetchable attachment (or no workspace
    /// directory is configured for downloads). Not a failure.
    None,
    /// Attachment content was built successfully.
    Ready(String),
    /// Building the attachment failed for a condition that may clear up
    /// on its own (see `AttachmentBuildFailure::Retryable`).
    Retryable,
    /// Building the attachment failed for a condition retrying will not
    /// fix (see `AttachmentBuildFailure::Permanent`).
    Permanent,
}

/// Classify a local workspace filesystem error into a retry disposition.
///
/// Retrying the CDN cannot clear an unwritable workspace. If every local
/// I/O error is `Retryable`, the cursor is retained and the listener
/// re-fetches the same batch forever, holding every later WeChat message
/// behind a condition that will never resolve on its own. So errors that
/// need operator action are `Permanent`: the attachment is skipped, the
/// batch commits, and inbound delivery keeps flowing.
///
/// Genuinely transient conditions — a temporarily unavailable mount, a
/// full disk, an interrupted call — stay `Retryable` and keep the
/// existing bounded backoff.
fn classify_workspace_io(err: &std::io::Error) -> AttachmentDisposition {
    use std::io::ErrorKind;
    match err.kind() {
        // Operator-action conditions: permissions, a read-only mount, a
        // workspace path that is not (or is not under) a directory, a
        // malformed path, or a path already occupied by a conflicting
        // entry. None of these change by retrying.
        //
        // `AlreadyExists` is the EEXIST that `create_dir_all` returns when
        // the attachment directory path is occupied by a regular file: no
        // amount of re-fetching from the CDN turns that file into a
        // directory, so treating it as transient would wedge every later
        // inbound message behind the held batch.
        ErrorKind::PermissionDenied
        | ErrorKind::ReadOnlyFilesystem
        | ErrorKind::NotADirectory
        | ErrorKind::IsADirectory
        | ErrorKind::AlreadyExists
        | ErrorKind::InvalidInput
        | ErrorKind::InvalidFilename => AttachmentDisposition::Permanent,
        // Transient: unavailable mount, ENOSPC, EINTR, and anything the
        // std mapping does not name.
        _ => AttachmentDisposition::Retryable,
    }
}

/// ERROR-level, operator-visible record of a workspace write that cannot
/// be retried away, naming the path and the decision taken.
fn record_workspace_io_failure(
    path: &Path,
    err: &std::io::Error,
    disposition: &AttachmentDisposition,
    what: &str,
) {
    let permanent = matches!(disposition, AttachmentDisposition::Permanent);
    let decision = if permanent {
        "attachment skipped permanently; batch will commit so inbound delivery keeps flowing"
    } else {
        "attachment held for retry; batch cursor retained"
    };
    let attrs = ::serde_json::json!({
        "error": format!("{err}"),
        "error_kind": format!("{:?}", err.kind()),
        "workspace_path": path.display().to_string(),
        "permanent": permanent,
        "decision": decision,
    });
    if permanent {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(attrs),
            what
        );
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(attrs),
            what
        );
    }
}

/// Why downloading/decrypting an inbound attachment failed, classified
/// so the caller can pick a retryable-vs-permanent `AttachmentDisposition`.
///
/// Network and transport-layer failures are `Retryable`: connection
/// errors, request timeouts, CDN HTTP 5xx, CDN HTTP 429, and CDN HTTP
/// 408. Content and parse-layer failures are `Permanent`: CDN HTTP 4xx
/// other than 408/429 (the object is gone), decrypt/decode failures, and
/// unsupported/oversized content. When an error does not map cleanly to
/// either bucket, the default is `Retryable` for network/transport-layer
/// errors and `Permanent` for content/parse-layer errors.
///
/// Local workspace I/O is not classified here — it is classified by
/// `classify_workspace_io`, which splits transient conditions from ones
/// only an operator can clear.
#[derive(Debug)]
enum AttachmentBuildFailure {
    Retryable(String),
    Permanent(String),
}

impl std::fmt::Display for AttachmentBuildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(msg) | Self::Permanent(msg) => write!(f, "{msg}"),
        }
    }
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

/// Interpret an iLink `sendmessage` response body, returning a description
/// of the failure when the API reported one.
///
/// The iLink API reports send failures as HTTP 200 with a non-zero
/// `ret`/`errcode` in the JSON body — the same envelope the getUpdates
/// sync loop parses. Checking only the HTTP status treats those failures
/// (e.g. an expired or missing `context_token`) as success, so the message
/// is silently dropped.
///
/// An empty or non-JSON 2xx body carries no envelope to inspect and is
/// treated as success, preserving the pre-check behavior for those shapes.
fn sendmessage_body_error(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }
    let Ok(data) = serde_json::from_str::<serde_json::Value>(body) else {
        return None;
    };
    let ret = data.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
    let errcode = data.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
    if ret == 0 && errcode == 0 {
        return None;
    }
    let errmsg = data.get("errmsg").and_then(|v| v.as_str()).unwrap_or("");
    Some(format!("ret={ret}, errcode={errcode}, errmsg={errmsg:?}"))
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

    fn build_inbound_channel_message(
        &self,
        from_user_id: &str,
        message_id: String,
        text: &str,
        timestamp: u64,
        attachment_content: Option<String>,
    ) -> Option<Box<ChannelMessage>> {
        let content = match (attachment_content, text.is_empty()) {
            (Some(marker), true) => marker,
            (Some(marker), false) => format!("{marker}\n\n{text}"),
            (None, false) => text.to_string(),
            (None, true) => return None,
        };

        Some(Box::new(ChannelMessage {
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
        }))
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
    ) -> Result<Vec<u8>, AttachmentBuildFailure> {
        let resp = self
            .client
            .get(self.cdn_download_url(&spec.encrypted_query_param))
            .timeout(API_TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                // Connection error or request timeout: transport-layer,
                // may succeed on retry.
                AttachmentBuildFailure::Retryable(format!("attachment request failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let message = format!("attachment download failed ({status}): {body}");
            // CDN 5xx, 429, and 408 are treated as transient; every other
            // 4xx (e.g. 404/410) means the object is gone and won't come back.
            return Err(
                if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
                    AttachmentBuildFailure::Retryable(message)
                } else {
                    AttachmentBuildFailure::Permanent(message)
                },
            );
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| {
                AttachmentBuildFailure::Retryable(format!("attachment body read failed: {e}"))
            })?
            .to_vec();
        if bytes.len() as u64 > WECHAT_MEDIA_MAX_BYTES {
            // Oversized content: unsupported, won't shrink on retry.
            return Err(AttachmentBuildFailure::Permanent(format!(
                "inbound attachment exceeds {} MB limit",
                WECHAT_MEDIA_MAX_BYTES / (1024 * 1024)
            )));
        }

        match spec.aes_key.as_deref() {
            Some(aes_key) if !aes_key.is_empty() => {
                // Decrypt/decode failures are content-layer: corrupt or
                // undecryptable content will not fix itself on retry.
                let key = parse_aes_key(aes_key)
                    .map_err(|e| AttachmentBuildFailure::Permanent(e.to_string()))?;
                decrypt_aes_ecb(&bytes, &key)
                    .map_err(|e| AttachmentBuildFailure::Permanent(e.to_string()))
            }
            _ => Ok(bytes),
        }
    }

    async fn try_build_attachment_content(
        &self,
        items: &[serde_json::Value],
        message_id: &str,
    ) -> AttachmentDisposition {
        let Some(workspace_dir) = self.workspace_dir.as_ref() else {
            return AttachmentDisposition::None;
        };
        let Some(spec) = Self::find_inbound_attachment(items, message_id) else {
            return AttachmentDisposition::None;
        };
        let bytes = match self.download_inbound_attachment(&spec).await {
            Ok(bytes) => bytes,
            Err(failure) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "error": failure.to_string(),
                            "retryable": matches!(failure, AttachmentBuildFailure::Retryable(_)),
                        })),
                    "attachment download skipped"
                );
                return match failure {
                    AttachmentBuildFailure::Retryable(_) => AttachmentDisposition::Retryable,
                    AttachmentBuildFailure::Permanent(_) => AttachmentDisposition::Permanent,
                };
            }
        };

        let save_dir = workspace_dir.join("wechat_files");
        if let Err(err) = tokio::fs::create_dir_all(&save_dir).await {
            // Local filesystem, not the CDN: a permission problem, a
            // read-only mount, or a non-directory workspace path is not
            // cleared by retrying, and holding the cursor for it wedges
            // every later message behind this batch.
            let disposition = classify_workspace_io(&err);
            record_workspace_io_failure(
                &save_dir,
                &err,
                &disposition,
                "failed to create WeChat attachment dir",
            );
            return disposition;
        }

        let local_path = save_dir.join(&spec.file_name);
        if let Err(err) = tokio::fs::write(&local_path, bytes).await {
            // Same policy as the directory create above.
            let disposition = classify_workspace_io(&err);
            record_workspace_io_failure(
                &local_path,
                &err,
                &disposition,
                "failed to save WeChat attachment",
            );
            return disposition;
        }

        AttachmentDisposition::Ready(format_attachment_content(
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

        // The API reports failures as HTTP 200 with a non-zero ret/errcode
        // in the body; a status check alone silently drops the message.
        let body = resp
            .text()
            .await
            .context("failed to read sendMessage response body")?;
        if let Some(err) = sendmessage_body_error(&body) {
            anyhow::bail!("sendMessage failed ({err})");
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
        // Consecutive polls whose batch was held back by a retryable
        // attachment failure. Drives the backoff at the commit site and
        // resets on any committed batch.
        let mut consecutive_attachment_holds: u32 = 0;

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

            // Set when any message in this batch hit a retryable
            // attachment-build failure (see `AttachmentDisposition::Retryable`).
            // Checked at the commit site below: a retryable failure must
            // withhold `next_cursor` so the whole batch is re-fetched, and
            // the attachment retried, after a restart. A permanent failure
            // does not set this: holding the cursor forever for an
            // attachment that will never succeed would wedge the listener.
            let mut batch_has_retryable_attachment_failure = false;

            // Everything the batch wants to do downstream is staged in batch
            // order. Nothing is published to `tx` until every attachment the
            // sender is currently authorized to fetch has prepared cleanly.
            // Messages that may become authorized through an earlier staged
            // `/bind` retain only their transport fields; their attachment I/O
            // runs in the authorization-aware preparation phase below. This
            // prevents unauthenticated CDN/workspace side effects while still
            // keeping every inbound agent turn unpublished until the batch is
            // complete.
            enum StagedInbound {
                /// An authorized message, fully prepared and ready to
                /// publish downstream.
                Deliver(Box<ChannelMessage>),
                /// A message that follows a syntactically valid `/bind` from
                /// the same sender in this batch. Its
                /// attachment is deliberately NOT fetched or written while
                /// the sender is unauthorized. Before publication, the
                /// earlier bind runs, authorization is resolved again from
                /// the canonical Config-backed peer resolver, and only then
                /// may attachment preparation cross the CDN/workspace trust
                /// boundary.
                DeliverIfAuthorized {
                    from_user_id: String,
                    items: Vec<serde_json::Value>,
                    message_id: String,
                    text: String,
                    timestamp: u64,
                },
                /// A message from an unauthorized sender. Handling it has
                /// side effects (pairing attempts, outbound replies), so it
                /// is deferred until the authorization-aware preparation
                /// phase. A successful bind may therefore update canonical
                /// authorization before a dependent attachment later reports
                /// a retryable failure. The cursor remains pending, but the
                /// replay sees that canonical authorization and treats the
                /// already-applied `/bind` as a control no-op, preventing a
                /// second attempt or reply.
                Unauthorized { from_user_id: String, text: String },
                /// A control message or empty message that has already been
                /// handled during authorization-aware preparation and must
                /// not be published downstream.
                Skip,
            }
            let mut staged: Vec<StagedInbound> = Vec::new();
            // Ephemeral materialized view of possible authorization
            // transitions inside this one batch. It is not authorization
            // state: the preparation phase always re-resolves the canonical
            // Config-backed peer list before delivery or attachment I/O. Its
            // sole purpose is to defer later messages until a preceding
            // `/bind` has been evaluated in message order.
            let mut staged_bind_senders = std::collections::HashSet::new();

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

                // Resolve current authorization from the canonical peer
                // configuration. A prior `/bind` from this sender in the
                // same batch is only a possible transition: prepare this
                // message's transport metadata now, but do not fetch its
                // attachment. Re-check the canonical source after that bind
                // runs, before crossing the CDN/workspace trust boundary.
                let currently_authorized = self.is_user_allowed(from_user_id);
                let may_be_authorized_by_staged_bind = staged_bind_senders.contains(from_user_id);
                if currently_authorized && Self::extract_bind_code(&text).is_some() {
                    // A held batch can be replayed after its bind already
                    // succeeded but before the attachment and cursor commit.
                    // Treat that replayed control message as a no-op: it must
                    // not reach the agent and must not repeat the pairing
                    // attempt or success reply.
                    continue;
                }
                if !currently_authorized && !may_be_authorized_by_staged_bind {
                    if Self::extract_bind_code(&text).is_some() {
                        staged_bind_senders.insert(from_user_id.to_string());
                    }
                    staged.push(StagedInbound::Unauthorized {
                        from_user_id: from_user_id.to_string(),
                        text,
                    });
                    continue;
                }

                let timestamp = msg
                    .get("create_time_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    / 1000; // Convert to seconds

                if !currently_authorized {
                    // The sender is not authorized yet, so even a syntactically
                    // valid preceding bind cannot authorize network or
                    // filesystem side effects. Keep the raw transport fields
                    // ephemeral until publication applies the bind and the
                    // canonical peer resolver confirms the transition.
                    staged.push(StagedInbound::DeliverIfAuthorized {
                        from_user_id: from_user_id.to_string(),
                        items,
                        message_id,
                        text,
                        timestamp,
                    });
                    continue;
                }

                let attachment_content =
                    match self.try_build_attachment_content(&items, &message_id).await {
                        AttachmentDisposition::Ready(marker) => Some(marker),
                        AttachmentDisposition::None | AttachmentDisposition::Permanent => None,
                        AttachmentDisposition::Retryable => {
                            // Defer the whole batch. The cursor is held
                            // below, so this exact batch is re-fetched on
                            // the next poll. The messages staged so far are
                            // discarded — NOT published — because the held
                            // cursor means they will be re-fetched and
                            // re-staged on the next pass; publishing them
                            // now would deliver them again on every pass
                            // for as long as the failure persists, and each
                            // duplicate can start another agent turn and
                            // repeat downstream tool effects.
                            batch_has_retryable_attachment_failure = true;
                            break;
                        }
                    };
                if let Some(channel_msg) = self.build_inbound_channel_message(
                    from_user_id,
                    message_id,
                    &text,
                    timestamp,
                    attachment_content,
                ) {
                    staged.push(StagedInbound::Deliver(channel_msg));
                }
            }

            // Apply authorization transitions and prepare their dependent
            // messages before ANY message crosses `tx.send`. This second
            // preparation phase is necessary because attachment I/O for an
            // unauthorized sender must not run merely because a preceding
            // message looks like `/bind CODE`: only the canonical peer source
            // after `try_pair` may authorize that CDN request and workspace
            // write.
            if !batch_has_retryable_attachment_failure {
                for item in &mut staged {
                    let pending = std::mem::replace(item, StagedInbound::Skip);
                    match pending {
                        StagedInbound::Deliver(message) => {
                            *item = StagedInbound::Deliver(message);
                        }
                        StagedInbound::Unauthorized { from_user_id, text } => {
                            self.handle_unauthorized_message(&from_user_id, &text).await;
                        }
                        StagedInbound::DeliverIfAuthorized {
                            from_user_id,
                            items,
                            message_id,
                            text,
                            timestamp,
                        } => {
                            if !self.is_user_allowed(&from_user_id) {
                                // The preceding bind was invalid or could
                                // not make this sender canonical. Fail
                                // closed without fetching or persisting the
                                // attachment.
                                self.handle_unauthorized_message(&from_user_id, &text).await;
                                continue;
                            }

                            let attachment_content = match self
                                .try_build_attachment_content(&items, &message_id)
                                .await
                            {
                                AttachmentDisposition::Ready(marker) => Some(marker),
                                AttachmentDisposition::None | AttachmentDisposition::Permanent => {
                                    None
                                }
                                AttachmentDisposition::Retryable => {
                                    // The bind has already updated the
                                    // canonical peer source, but no inbound
                                    // message has crossed `tx.send`. Hold the
                                    // cursor and replay. On that replay the
                                    // now-authorized `/bind` is a control
                                    // no-op, so its one-time code and reply
                                    // are not repeated.
                                    batch_has_retryable_attachment_failure = true;
                                    break;
                                }
                            };

                            if let Some(message) = self.build_inbound_channel_message(
                                &from_user_id,
                                message_id,
                                &text,
                                timestamp,
                                attachment_content,
                            ) {
                                *item = StagedInbound::Deliver(message);
                            }
                        }
                        StagedInbound::Skip => {}
                    }
                }
            }

            // Publish only after the WHOLE batch prepared cleanly. A
            // retryable failure discards the staged messages instead —
            // they are re-fetched with the held cursor on the next pass,
            // so no inbound agent turn is delivered twice.
            if !batch_has_retryable_attachment_failure {
                for item in staged {
                    let StagedInbound::Deliver(channel_msg) = item else {
                        continue;
                    };
                    if tx.send(*channel_msg).await.is_err() {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            "channel receiver dropped, stopping"
                        );
                        // Do NOT commit `next_cursor` here: the batch is
                        // only partially (or not at all) enqueued, so the
                        // old cursor must stay on disk. On supervised
                        // restart `listen()` reloads it and re-polls this
                        // batch.
                        return Ok(());
                    }
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
            //
            // A batch that hit a retryable attachment failure is held back
            // the same way: committing `next_cursor` here would permanently
            // skip the attachment on restart, since the batch would never
            // be re-fetched. A permanent attachment failure does not hold
            // the cursor: the attachment is genuinely unfetchable, and
            // holding forever would wedge the listener on it.
            if batch_has_retryable_attachment_failure {
                // The batch is deferred, not dropped: preparation broke at
                // the failing message, the staged messages were discarded
                // unpublished, and the cursor stays put — so the next poll
                // re-fetches the identical batch and NOTHING from this
                // pass was delivered. Back off before re-polling so a
                // sustained CDN outage cannot spin this loop; the delay
                // doubles per consecutive held pass up to a ceiling.
                let delay = ATTACHMENT_RETRY_BASE_DELAY
                    .saturating_mul(1u32 << consecutive_attachment_holds.min(5))
                    .min(ATTACHMENT_RETRY_MAX_DELAY);
                consecutive_attachment_holds = consecutive_attachment_holds.saturating_add(1);
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "consecutive_attachment_holds": consecutive_attachment_holds,
                            "delay_ms": delay.as_millis() as u64,
                        })),
                    "holding WeChat batch after retryable attachment failure; backing off before re-poll"
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            consecutive_attachment_holds = 0;
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

    fn test_wechat_channel_for_api(api_base_url: String, state_dir: &Path) -> WeChatChannel {
        let mut channel = WeChatChannel::new(
            "wechat_test_alias",
            Arc::new(|| vec!["*".into()]),
            None,
            None,
            Some(state_dir.to_path_buf()),
        )
        .unwrap();
        channel.api_base_url = api_base_url;
        *channel.bot_token.write().unwrap() = Some("test-token".into());
        channel
    }

    #[tokio::test]
    async fn send_text_reports_2xx_error_envelope() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ret": -1,
                "errcode": 301,
                "errmsg": "context token expired"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let state = tempdir().unwrap();
        let channel = test_wechat_channel_for_api(server.uri(), state.path());
        let err = channel
            .send_text("recipient", "hello", None)
            .await
            .expect_err("a 2xx iLink error envelope must fail the send");

        let message = err.to_string();
        assert!(message.contains("sendMessage failed"), "{message}");
        assert!(message.contains("errcode=301"), "{message}");
    }

    #[tokio::test]
    async fn send_text_propagates_2xx_body_read_failure() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
        });

        let state = tempdir().unwrap();
        let channel = test_wechat_channel_for_api(format!("http://{address}"), state.path());
        let err = tokio::time::timeout(
            Duration::from_secs(5),
            channel.send_text("recipient", "hello", None),
        )
        .await
        .expect("the local truncated-body request must complete")
        .expect_err("a truncated 2xx response body must fail the send");
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the local truncated-body server must complete")
            .unwrap();

        assert!(
            err.to_string()
                .contains("failed to read sendMessage response body"),
            "{err:#}"
        );
    }

    #[test]
    fn sendmessage_body_error_flags_nonzero_ret() {
        let err = sendmessage_body_error(r#"{"ret":-1,"errmsg":"context token expired"}"#)
            .expect("non-zero ret must be reported as an error");
        assert!(err.contains("ret=-1"), "ret code missing from error: {err}");
        assert!(
            err.contains("context token expired"),
            "errmsg missing from error: {err}"
        );
    }

    #[test]
    fn sendmessage_body_error_flags_nonzero_errcode() {
        let err = sendmessage_body_error(r#"{"ret":0,"errcode":301,"errmsg":"session expired"}"#)
            .expect("non-zero errcode must be reported as an error");
        assert!(err.contains("errcode=301"), "errcode missing: {err}");
    }

    #[test]
    fn sendmessage_body_error_accepts_success_envelope() {
        assert_eq!(
            sendmessage_body_error(r#"{"ret":0,"errcode":0,"errmsg":""}"#),
            None
        );
        // Fields absent entirely also means success (defaults are 0).
        assert_eq!(sendmessage_body_error(r#"{"msg_id":"abc"}"#), None);
    }

    #[test]
    fn sendmessage_body_error_preserves_legacy_success_for_empty_or_non_json() {
        // An empty 2xx body was success before this check existed; keep it so.
        assert_eq!(sendmessage_body_error(""), None);
        assert_eq!(sendmessage_body_error("   "), None);
        // A non-JSON 2xx body has no envelope to inspect; do not invent failures.
        assert_eq!(sendmessage_body_error("OK"), None);
    }

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

    /// Like `wechat_channel_for_mock`, but also points the CDN base URL at
    /// the mock server and configures a workspace directory so inbound
    /// attachments can be downloaded and saved.
    fn wechat_channel_for_mock_with_workspace(
        state_dir: PathBuf,
        workspace_dir: PathBuf,
        mock_base_url: String,
    ) -> WeChatChannel {
        let mut ch = wechat_channel_for_mock(state_dir, mock_base_url.clone());
        ch.cdn_base_url = mock_base_url;
        ch.with_workspace_dir(workspace_dir)
    }

    /// Build the production pairing shape: the resolver and persistence
    /// writer share one canonical Config handle, initially with no peers.
    /// The returned code is the one-time `/bind` code generated by
    /// `WeChatChannel::new` for that empty allowlist.
    fn pairing_wechat_channel_for_mock(
        root: &Path,
        mock_base_url: String,
    ) -> (WeChatChannel, Arc<parking_lot::RwLock<Config>>, String) {
        let alias = "wechat_test_alias";
        let mut config = Config {
            config_path: root.join("config.toml"),
            data_dir: root.join("data"),
            ..Default::default()
        };
        config.channels.wechat.insert(
            alias.to_string(),
            zeroclaw_config::schema::WeChatConfig {
                enabled: true,
                ..Default::default()
            },
        );
        let config = Arc::new(parking_lot::RwLock::new(config));
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let config = config.clone();
            let alias = alias.to_string();
            Arc::new(move || config.read().channel_external_peers("wechat", &alias))
        };

        let mut channel =
            WeChatChannel::new(alias, peer_resolver, None, None, Some(root.join("state")))
                .unwrap()
                .with_persistence(config.clone())
                .with_workspace_dir(root.join("workspace"));
        let pairing_code = channel
            .pairing
            .as_ref()
            .and_then(PairingGuard::pairing_code)
            .expect("empty canonical peer list must generate a pairing code");
        channel.api_base_url = mock_base_url.clone();
        channel.cdn_base_url = mock_base_url;
        *channel.bot_token.write().unwrap() = Some("test-token".to_string());
        (channel, config, pairing_code)
    }

    /// A `/bind` changes authorization for the messages after it in the
    /// same server batch. Staging must not freeze the pre-bind decision:
    /// once the bind is persisted, the following message is delivered in
    /// order before the cursor commits.
    #[tokio::test]
    async fn listen_applies_bind_before_following_same_sender_message() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("workspace")).unwrap();
        let mock_server = MockServer::start().await;
        let (channel, config, pairing_code) =
            pairing_wechat_channel_for_mock(temp.path(), mock_server.uri());

        let batch = getupdates_batch(
            "cursor_after_batch",
            serde_json::json!([
                {
                    "from_user_id": "new_user",
                    "message_id": 1,
                    "create_time_ms": 1_700_000_000_000u64,
                    "item_list": [{
                        "type": 1,
                        "text_item": {"text": format!("/bind {pairing_code}")}
                    }]
                },
                {
                    "from_user_id": "new_user",
                    "message_id": 2,
                    "create_time_ms": 1_700_000_001_000u64,
                    "item_list": [{
                        "type": 1,
                        "text_item": {"text": "hello after bind"}
                    }]
                }
            ]),
        );
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "original_cursor"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(batch))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([]),
            )))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ret": 0})))
            .expect(1)
            .mount(&mock_server)
            .await;

        *channel.cursor.lock() = "original_cursor".to_string();
        channel.save_sync_data();
        let channel = Arc::new(channel);
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let listen_channel = channel.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_channel.listen(tx).await });

        let delivered = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for the post-bind message")
            .expect("listener closed before the post-bind message");
        assert_eq!(delivered.sender, "new_user");
        assert_eq!(delivered.content, "hello after bind");
        assert_eq!(
            config
                .read()
                .channel_external_peers("wechat", "wechat_test_alias"),
            vec!["new_user".to_string()],
            "bind must update the canonical peer source before delivery"
        );

        let duplicate = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(duplicate.is_err(), "post-bind message must deliver once");
        assert_eq!(*channel.cursor.lock(), "cursor_after_batch");

        handle.abort();
        let _ = handle.await;
    }

    /// A syntactically valid `/bind` is only a staging hint, not authority.
    /// An invalid code followed by an attachment from the same sender must
    /// not trigger a CDN request or workspace write before canonical pairing
    /// succeeds.
    #[tokio::test]
    async fn listen_does_not_fetch_attachment_after_invalid_staged_bind() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("workspace")).unwrap();
        let mock_server = MockServer::start().await;
        let (channel, config, valid_pairing_code) =
            pairing_wechat_channel_for_mock(temp.path(), mock_server.uri());
        let invalid_pairing_code = format!("{valid_pairing_code}x");

        let batch = getupdates_batch(
            "cursor_after_batch",
            serde_json::json!([
                {
                    "from_user_id": "unpaired_user",
                    "message_id": 1,
                    "create_time_ms": 1_700_000_000_000u64,
                    "item_list": [{
                        "type": 1,
                        "text_item": {"text": format!("/bind {invalid_pairing_code}")}
                    }]
                },
                {
                    "from_user_id": "unpaired_user",
                    "message_id": 2,
                    "create_time_ms": 1_700_000_001_000u64,
                    "item_list": [
                        {"type": 1, "text_item": {"text": "unauthorized attachment"}},
                        {
                            "type": 2,
                            "image_item": {
                                "media": {"encrypt_query_param": "must_not_be_fetched"}
                            }
                        }
                    ]
                }
            ]),
        );
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "original_cursor"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(batch))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([]),
            )))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ret": 0})))
            .expect(1)
            .mount(&mock_server)
            .await;

        *channel.cursor.lock() = "original_cursor".to_string();
        channel.save_sync_data();
        let channel = Arc::new(channel);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listen_channel = channel.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_channel.listen(tx).await });

        tokio::time::timeout(Duration::from_secs(10), async {
            while *channel.cursor.lock() != "cursor_after_batch" {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("invalid bind batch must advance without attachment I/O");

        assert!(
            tokio::time::timeout(Duration::from_millis(250), rx.recv())
                .await
                .is_err(),
            "an invalid bind must not deliver its following attachment"
        );
        assert!(
            config
                .read()
                .channel_external_peers("wechat", "wechat_test_alias")
                .is_empty(),
            "invalid pairing must not update the canonical peer source"
        );
        assert!(
            !temp.path().join("workspace/wechat_files").exists(),
            "unauthorized attachment staging must not create its workspace directory"
        );
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method.as_str() == "GET")
                .count(),
            0,
            "invalid pairing must not authorize any CDN request"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/ilink/bot/sendmessage")
                .count(),
            1,
            "the invalid-code reply should be sent once"
        );

        handle.abort();
        let _ = handle.await;
    }

    /// Attachment I/O for a staged-bind sender starts only after the bind
    /// updates canonical authorization. If that newly authorized fetch is
    /// retryable, the cursor stays pending; replay treats the already-applied
    /// `/bind` as a control no-op so the pairing attempt and reply happen once,
    /// while the inbound message still waits for the attachment.
    #[tokio::test]
    async fn listen_replays_post_bind_attachment_failure_without_rebinding() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("workspace")).unwrap();
        let mock_server = MockServer::start().await;
        let (channel, config, pairing_code) =
            pairing_wechat_channel_for_mock(temp.path(), mock_server.uri());

        let batch = getupdates_batch(
            "cursor_after_batch",
            serde_json::json!([
                {
                    "from_user_id": "new_user",
                    "message_id": 1,
                    "create_time_ms": 1_700_000_000_000u64,
                    "item_list": [{
                        "type": 1,
                        "text_item": {"text": format!("/bind {pairing_code}")}
                    }]
                },
                {
                    "from_user_id": "new_user",
                    "message_id": 2,
                    "create_time_ms": 1_700_000_001_000u64,
                    "item_list": [
                        {"type": 1, "text_item": {"text": "caption after bind"}},
                        {
                            "type": 2,
                            "image_item": {
                                "media": {"encrypt_query_param": "enc_param_1"}
                            }
                        }
                    ]
                }
            ]),
        );
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "original_cursor"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(batch))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([]),
            )))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-image-bytes".to_vec()))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ret": 0})))
            .expect(1)
            .mount(&mock_server)
            .await;

        *channel.cursor.lock() = "original_cursor".to_string();
        channel.save_sync_data();
        let channel = Arc::new(channel);
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let listen_channel = channel.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_channel.listen(tx).await });

        let held = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
        assert!(held.is_err(), "held batch must publish no message");
        assert_eq!(
            config
                .read()
                .channel_external_peers("wechat", "wechat_test_alias"),
            vec!["new_user".to_string()],
            "the bind must establish canonical authorization before attachment I/O"
        );
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/ilink/bot/sendmessage")
                .count(),
            1,
            "the successful bind reply must be sent once before the held replay"
        );

        let delivered = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("timed out waiting for attachment recovery")
            .expect("listener closed before attachment recovery");
        assert_eq!(delivered.sender, "new_user");
        assert!(delivered.content.contains("[IMAGE:"), "{delivered:?}");
        assert!(delivered.content.contains("caption after bind"));
        assert_eq!(
            config
                .read()
                .channel_external_peers("wechat", "wechat_test_alias"),
            vec!["new_user".to_string()]
        );

        let duplicate = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(duplicate.is_err(), "recovered message must deliver once");
        assert_eq!(*channel.cursor.lock(), "cursor_after_batch");
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/ilink/bot/sendmessage")
                .count(),
            1,
            "replayed bind must reply exactly once"
        );

        handle.abort();
        let _ = handle.await;
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

    /// Invariant: a batch carrying a text-plus-attachment message whose
    /// attachment download fails retryably (a transient CDN 503) must not
    /// let `next_cursor` commit, and must publish nothing while held.
    /// Delivering the bare text and moving the cursor on would permanently
    /// drop the attachment, since a redelivered message never happens
    /// without inbound dedup.
    ///
    /// Scope: SAME-PROCESS recovery. One long-lived listener stays up
    /// across the failure and the CDN's recovery, so this covers the
    /// held-then-recovered path plus the post-commit persistence a later
    /// restart would read. It deliberately does NOT cover restart *while*
    /// the cursor is still pending — that is
    /// `listen_recovers_held_batch_after_restart_while_cursor_pending`.
    #[tokio::test]
    async fn listen_holds_cursor_on_retryable_attachment_failure_and_recovers_on_same_listener() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let mock_server = MockServer::start().await;

        let attachment_batch = || {
            getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "item_list": [
                            {"type": 1, "text_item": {"text": "hello"}},
                            {
                                "type": 2,
                                "image_item": {
                                    "media": {"encrypt_query_param": "enc_param_1"}
                                }
                            }
                        ]
                    }
                ]),
            )
        };

        // getupdates: the batch is served while the listener still polls
        // with the pre-batch cursor. Once the batch commits and the
        // listener polls with `cursor_after_batch`, the server has nothing
        // left — mirroring a real server, which only replays a batch the
        // client has not acknowledged.
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "original_cursor"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(attachment_batch()))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([]),
            )))
            .mount(&mock_server)
            .await;

        // CDN: retryable failure (503) on the first download attempt,
        // then succeeds on every attempt after.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-image-bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        );
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // The first pass hits the 503 and holds the batch, so nothing is
        // delivered yet — in particular the bare "hello" text must NOT be
        // enqueued, or it would repeat on every re-poll.
        let held = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(
            held.is_err(),
            "a retryable attachment failure must defer the batch, not deliver degraded content"
        );

        // Keep the SAME listener running across the failure and into
        // recovery — this is the production path a restart-only test skips.
        // The CDN mock succeeds after the first attempt, so the backed-off
        // re-poll re-fetches the same batch and delivers it complete, exactly
        // once.
        let first = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("timed out waiting for the recovered delivery")
            .expect("channel closed before the recovered delivery");
        assert_eq!(first.sender, "user_a");
        assert!(
            first.content.contains("[IMAGE:"),
            "the recovered pass must carry the attachment, got: {}",
            first.content
        );
        assert!(
            first.content.contains("hello"),
            "the recovered pass must still carry the text, got: {}",
            first.content
        );

        // And the batch must not be delivered a second time once its
        // cursor commits.
        let duplicate = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(
            duplicate.is_err(),
            "a recovered batch must deliver exactly once, not redeliver on the next poll"
        );

        handle.abort();
        let _ = handle.await;

        // The batch committed in-process once the CDN recovered, so a
        // supervised restart must not re-poll it.
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
            "cursor_after_batch",
            "cursor must advance once the held batch finally delivers in full"
        );
    }

    /// Unit-level companion to the listener regression below: the
    /// classification split itself. Conditions only an operator can clear
    /// are `Permanent`; genuinely transient ones stay `Retryable`.
    #[test]
    fn classify_workspace_io_splits_operator_action_from_transient() {
        use std::io::{Error, ErrorKind};
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::ReadOnlyFilesystem,
            ErrorKind::NotADirectory,
            ErrorKind::IsADirectory,
            ErrorKind::AlreadyExists,
            ErrorKind::InvalidInput,
            ErrorKind::InvalidFilename,
        ] {
            assert_eq!(
                classify_workspace_io(&Error::new(kind, "boom")),
                AttachmentDisposition::Permanent,
                "{kind:?} cannot be cleared by retrying the CDN"
            );
        }
        for kind in [
            ErrorKind::StorageFull,
            ErrorKind::Interrupted,
            ErrorKind::TimedOut,
            ErrorKind::Other,
        ] {
            assert_eq!(
                classify_workspace_io(&Error::new(kind, "boom")),
                AttachmentDisposition::Retryable,
                "{kind:?} may clear on its own"
            );
        }
    }

    /// Invariant: an unwritable workspace must not wedge inbound
    /// delivery. Every local filesystem error used to map to
    /// `Retryable`, so a `PermissionDenied` workspace held the cursor and
    /// re-fetched the same batch forever — every later WeChat message
    /// stuck behind a condition retrying can never clear.
    ///
    /// Here the workspace is a read-only directory, so `create_dir_all`
    /// of `wechat_files` fails with `PermissionDenied` on every pass. The
    /// batch must be classified permanent: the attachment is dropped, the
    /// text still delivers, and the cursor commits so the next batch can
    /// flow.
    #[tokio::test]
    #[cfg(unix)]
    async fn listen_does_not_hold_batch_forever_on_unwritable_workspace() {
        use std::os::unix::fs::PermissionsExt;
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        // Read-only workspace: creating `wechat_files/` inside it is
        // EACCES, and no amount of CDN retrying changes that.
        let mut perms = std::fs::metadata(&workspace_dir).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&workspace_dir, perms).unwrap();

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "original_cursor"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "item_list": [
                            {"type": 1, "text_item": {"text": "hello"}},
                            {
                                "type": 2,
                                "image_item": {
                                    "media": {"encrypt_query_param": "enc_param_1"}
                                }
                            }
                        ]
                    }
                ]),
            )))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([]),
            )))
            .mount(&mock_server)
            .await;
        // The CDN is healthy throughout: the only failure is local.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-image-bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        );
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // Delivery must not be wedged: the text arrives promptly, well
        // inside the first backoff step, instead of being held.
        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("an unwritable workspace must not hold inbound delivery")
            .expect("channel closed before delivery");
        assert_eq!(msg.sender, "user_a");
        assert!(
            msg.content.contains("hello"),
            "the text must still be delivered, got: {}",
            msg.content
        );
        assert!(
            !msg.content.contains("[IMAGE:"),
            "the attachment is unsaveable, so no marker may be claimed, got: {}",
            msg.content
        );

        // And the cursor must move on, or the next batch never arrives.
        let mut committed = false;
        for _ in 0..100 {
            if *ch.cursor.lock() == "cursor_after_batch" {
                committed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            committed,
            "an unclearable local I/O failure must not retain the cursor: still at {:?}",
            *ch.cursor.lock()
        );

        handle.abort();
        let _ = handle.await;

        // Restore write permission so the tempdir can be cleaned up.
        let mut perms = std::fs::metadata(&workspace_dir).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&workspace_dir, perms).unwrap();
    }

    /// Same invariant as the read-only-workspace regression above, for
    /// the collision that needs no permission trick and so reproduces on
    /// every platform: `workspace/wechat_files` already exists as a
    /// regular file.
    ///
    /// `create_dir_all` then returns EEXIST (`ErrorKind::AlreadyExists`).
    /// Retrying the CDN cannot turn a file into a directory, so if this
    /// were classified `Retryable` the listener would retain the cursor
    /// and re-poll the same batch forever, wedging every later inbound
    /// message. It must be `Permanent`: the attachment is dropped, the
    /// text still delivers, and the cursor commits.
    #[tokio::test]
    async fn listen_does_not_hold_batch_when_attachment_dir_path_is_a_file() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        // The collision: the attachment directory path is occupied by a
        // regular file, so `create_dir_all` fails with EEXIST forever.
        std::fs::write(workspace_dir.join("wechat_files"), b"not a directory").unwrap();

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "original_cursor"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "item_list": [
                            {"type": 1, "text_item": {"text": "hello"}},
                            {
                                "type": 2,
                                "image_item": {
                                    "media": {"encrypt_query_param": "enc_param_1"}
                                }
                            }
                        ]
                    }
                ]),
            )))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([]),
            )))
            .mount(&mock_server)
            .await;
        // The CDN is healthy throughout: the only failure is local.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-image-bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        );
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("an occupied attachment dir path must not hold inbound delivery")
            .expect("channel closed before delivery");
        assert_eq!(msg.sender, "user_a");
        assert!(
            msg.content.contains("hello"),
            "the text must still be delivered, got: {}",
            msg.content
        );
        assert!(
            !msg.content.contains("[IMAGE:"),
            "the attachment is unsaveable, so no marker may be claimed, got: {}",
            msg.content
        );

        let mut committed = false;
        for _ in 0..100 {
            if *ch.cursor.lock() == "cursor_after_batch" {
                committed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            committed,
            "an EEXIST attachment-dir collision must not retain the cursor: still at {:?}",
            *ch.cursor.lock()
        );

        // The colliding file is untouched: nothing tried to write through it.
        assert_eq!(
            std::fs::read(workspace_dir.join("wechat_files")).unwrap(),
            b"not a directory",
            "the colliding file must not be overwritten"
        );

        handle.abort();
        let _ = handle.await;
    }

    /// The restart-while-cursor-pending regression, and the gap the
    /// same-process test above does NOT cover: the process must be able
    /// to die *while the batch is still held* and have a freshly
    /// constructed channel — built through `WeChatChannel::new` from the
    /// persisted state dir, exactly like a supervised restart — re-poll
    /// that batch and deliver it once.
    ///
    /// Pass 1 holds the batch (CDN 503) and the listener is aborted
    /// before the cursor ever commits, so `sync.json` still carries the
    /// pre-batch cursor. Pass 2 rebuilds from that file with a healthy
    /// CDN and must re-fetch the identical batch, deliver it complete
    /// exactly once, and only then commit. A regression in
    /// `save_sync_data()` ordering, or in the reload constructor, breaks
    /// this even though every same-process test stays green.
    #[tokio::test]
    async fn listen_recovers_held_batch_after_restart_while_cursor_pending() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let mock_server = MockServer::start().await;

        let attachment_batch = || {
            getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "item_list": [
                            {"type": 1, "text_item": {"text": "hello"}},
                            {
                                "type": 2,
                                "image_item": {
                                    "media": {"encrypt_query_param": "enc_param_1"}
                                }
                            }
                        ]
                    }
                ]),
            )
        };

        // The server only replays a batch the client has not acknowledged:
        // polling with the pre-batch cursor yields the batch, polling with
        // the committed cursor yields nothing. That matcher is what makes
        // "delivered exactly once" meaningful below.
        async fn mount_getupdates(server: &MockServer, batch: serde_json::Value) {
            Mock::given(method("POST"))
                .and(path("/ilink/bot/getupdates"))
                .and(body_partial_json(
                    serde_json::json!({"get_updates_buf": "original_cursor"}),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(batch))
                .mount(server)
                .await;
            Mock::given(method("POST"))
                .and(path("/ilink/bot/getupdates"))
                .and(body_partial_json(
                    serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                    "cursor_after_batch",
                    serde_json::json!([]),
                )))
                .mount(server)
                .await;
        }

        // ---- pass 1: CDN down, batch held, killed before commit ----
        mount_getupdates(&mock_server, attachment_batch()).await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        );
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let held = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
        assert!(
            held.is_err(),
            "nothing may be delivered while the batch is held on a failing CDN"
        );

        // Kill the listener mid-hold — the crash/restart this test exists
        // for. The cursor has NOT committed at this point.
        handle.abort();
        let _ = handle.await;
        drop(rx);
        drop(ch);

        let pending = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        assert_eq!(
            *pending.cursor.lock(),
            "original_cursor",
            "the held batch must leave the pre-batch cursor on disk, or a restart skips it"
        );
        drop(pending);

        // ---- pass 2: restart from persisted state with a healthy CDN ----
        mock_server.reset().await;
        mount_getupdates(&mock_server, attachment_batch()).await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-image-bytes".to_vec()))
            .mount(&mock_server)
            .await;

        // Built through the production constructor, which reloads the
        // cursor from `sync.json`; the test never sets it by hand.
        let restarted = Arc::new(wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        ));
        assert_eq!(
            *restarted.cursor.lock(),
            "original_cursor",
            "the restarted channel must resume from the persisted pending cursor"
        );

        let (tx2, mut rx2) = tokio::sync::mpsc::channel(8);
        let restart_ch = restarted.clone();
        let handle2 = zeroclaw_spawn::spawn!(async move { restart_ch.listen(tx2).await });

        let msg = tokio::time::timeout(Duration::from_secs(20), rx2.recv())
            .await
            .expect("timed out waiting for the held batch to be redelivered after restart")
            .expect("channel closed before the held batch was redelivered");
        assert_eq!(msg.sender, "user_a");
        assert!(
            msg.content.contains("[IMAGE:"),
            "the restarted pass must carry the attachment, got: {}",
            msg.content
        );
        assert!(
            msg.content.contains("hello"),
            "the restarted pass must still carry the text, got: {}",
            msg.content
        );

        let duplicate = tokio::time::timeout(Duration::from_secs(3), rx2.recv()).await;
        assert!(
            duplicate.is_err(),
            "a batch recovered after restart must deliver exactly once"
        );

        handle2.abort();
        let _ = handle2.await;
        drop(rx2);
        drop(restarted);

        let committed = WeChatChannel::new(
            "test",
            Arc::new(|| vec!["*".to_string()]),
            None,
            None,
            Some(state_dir.clone()),
        )
        .unwrap();
        assert_eq!(
            *committed.cursor.lock(),
            "cursor_after_batch",
            "the cursor must commit once the restarted pass delivers the batch in full"
        );
    }

    /// Invariant: while a batch is held for a retryable attachment
    /// failure, NOTHING may be delivered — in particular an ordinary text
    /// message A that precedes the failing message B must not cross
    /// `tx.send`, or it would start a fresh agent turn on every held
    /// re-poll. Once the CDN recovers, A and B arrive exactly once, in
    /// order, with no duplicates afterward.
    #[tokio::test]
    async fn listen_stages_whole_batch_before_publishing_any_message() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let mock_server = MockServer::start().await;

        let two_message_batch = || {
            getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([
                    {
                        "from_user_id": "user_a",
                        "message_id": 1,
                        "create_time_ms": 1_700_000_000_000u64,
                        "item_list": [{"type": 1, "text_item": {"text": "plain text A"}}]
                    },
                    {
                        "from_user_id": "user_b",
                        "message_id": 2,
                        "create_time_ms": 1_700_000_001_000u64,
                        "item_list": [
                            {"type": 1, "text_item": {"text": "caption B"}},
                            {
                                "type": 2,
                                "image_item": {
                                    "media": {"encrypt_query_param": "enc_param_1"}
                                }
                            }
                        ]
                    }
                ]),
            )
        };

        // The server replays the batch for as long as the client polls
        // with the pre-batch cursor, and has nothing once it acknowledges.
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "original_cursor"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_message_batch()))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(
                serde_json::json!({"get_updates_buf": "cursor_after_batch"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(getupdates_batch(
                "cursor_after_batch",
                serde_json::json!([]),
            )))
            .mount(&mock_server)
            .await;

        // CDN: retryable failure (503) on the first attempt, success after.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-image-bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        );
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // While the batch is held by B's 503, NOTHING is delivered — not
        // even A, whose own preparation succeeded. Publishing A here would
        // redeliver it on every held re-poll.
        let held = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(
            held.is_err(),
            "message A must not be published while a later message holds the batch"
        );

        // After the backed-off re-poll the CDN succeeds: A then B arrive,
        // in order, on the same listener.
        let first = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("timed out waiting for message A after recovery")
            .expect("channel closed before message A");
        assert_eq!(first.sender, "user_a");
        assert_eq!(first.content, "plain text A");

        let second = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("timed out waiting for message B after recovery")
            .expect("channel closed before message B");
        assert_eq!(second.sender, "user_b");
        assert!(
            second.content.contains("[IMAGE:"),
            "message B must carry its recovered attachment, got: {}",
            second.content
        );
        assert!(
            second.content.contains("caption B"),
            "message B must still carry its text, got: {}",
            second.content
        );

        // Exactly once: the committed cursor means the next poll returns
        // an empty batch, so no duplicate of A or B may appear.
        let duplicate = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(
            duplicate.is_err(),
            "a recovered batch must deliver exactly once, got a duplicate"
        );

        handle.abort();
        let _ = handle.await;

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
            "cursor_after_batch",
            "cursor must advance once the held batch delivers in full"
        );
    }

    /// Companion to the retryable-failure regression above: a permanent
    /// attachment failure (CDN 404, object gone) must NOT hold the
    /// cursor. Holding it forever for an attachment that will never
    /// succeed would wedge the listener on that batch.
    #[tokio::test]
    async fn listen_advances_cursor_when_attachment_fails_permanently() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
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
                        "item_list": [
                            {"type": 1, "text_item": {"text": "hello"}},
                            {
                                "type": 2,
                                "image_item": {
                                    "media": {"encrypt_query_param": "enc_param_1"}
                                }
                            }
                        ]
                    }
                ]),
            )))
            .mount(&mock_server)
            .await;

        // CDN: the object is gone (404) on every attempt.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        );
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for message")
            .expect("channel closed before message");
        assert_eq!(first.sender, "user_a");
        assert_eq!(
            first.content, "hello",
            "a permanent attachment failure delivers the text without the attachment"
        );

        handle.abort();
        let _ = handle.await;

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
            "cursor_after_batch",
            "a permanent attachment failure must not wedge the listener by holding the cursor"
        );
    }

    /// A CDN `408 Request Timeout` is a transient delivery failure, not a
    /// missing object, so it must hold the cursor like a 5xx/429 rather
    /// than advancing past the attachment and losing it forever.
    #[tokio::test]
    async fn listen_holds_cursor_when_attachment_download_times_out() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
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
                        "item_list": [
                            {"type": 1, "text_item": {"text": "hello"}},
                            {
                                "type": 2,
                                "image_item": {
                                    "media": {"encrypt_query_param": "enc_param_1"}
                                }
                            }
                        ]
                    }
                ]),
            )))
            .mount(&mock_server)
            .await;

        // CDN: transient timeout on every attempt.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(408))
            .mount(&mock_server)
            .await;

        let ch = wechat_channel_for_mock_with_workspace(
            state_dir.clone(),
            workspace_dir.clone(),
            mock_server.uri(),
        );
        *ch.cursor.lock() = "original_cursor".to_string();
        ch.save_sync_data();
        let ch = Arc::new(ch);

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // A held batch must not deliver anything: the message carries an
        // attachment that never arrives, so enqueueing its bare text would
        // redeliver that text on every re-poll for as long as the 408
        // persists. Nothing should come through.
        let delivered = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(
            delivered.is_err(),
            "a retryable attachment failure must defer the batch, not deliver degraded content"
        );

        handle.abort();
        let _ = handle.await;

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
            "a 408 is transient: the cursor must stay pending so the attachment is re-fetched"
        );
    }
}
