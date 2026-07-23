use anyhow::Context;
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use reqwest::multipart::{Form, Part};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::{Config, StreamMode, TELEGRAM_OFFICIAL_API_BASE_URL};
use zeroclaw_runtime::security::pairing::PairingGuard;

/// Telegram's maximum message length for text messages
const TELEGRAM_MAX_MESSAGE_LENGTH: usize = 4096;
const TELEGRAM_CONTINUED_PREFIX: &str = "(continued)\n\n";
const TELEGRAM_CONTINUES_SUFFIX: &str = "\n\n(continues...)";
const TELEGRAM_FENCE_REOPEN: &str = "```\n";
const TELEGRAM_FENCE_CLOSE: &str = "```";
const TELEGRAM_ACK_REACTIONS: &[&str] = &["⚡️", "👌", "👀", "🔥", "👍"];
const TELEGRAM_MEDIA_GROUP_SETTLE_DELAY: Duration = Duration::from_millis(700);
const TELEGRAM_IDLE_POLL_TIMEOUT_SECS: u64 = 30;
const TELEGRAM_PENDING_MEDIA_GROUP_POLL_TIMEOUT_SECS: u64 = 1;

type MediaGroupKey = (i64, String);

/// Raw, not-yet-dispatched updates for one Telegram media group.
///
/// The map that owns these values is deliberately local to `listen`: it is
/// the canonical transient state only while an album is waiting to settle.
#[derive(Debug)]
struct PendingMediaGroup {
    updates: Vec<serde_json::Value>,
    last_seen: Instant,
    last_seen_poll_generation: u64,
}

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
const TELEGRAM_BIND_COMMAND: &str = "/bind";
/// Telegram Bot API allows at most 100 commands via setMyCommands.
const TELEGRAM_MAX_BOT_COMMANDS: usize = 100;
/// Telegram command names: 1-32 lowercase a-z, 0-9, and underscore.
const TELEGRAM_COMMAND_NAME_MAX_LEN: usize = 32;
/// Telegram command descriptions nominally allow up to 256 characters per the API docs,
/// but empirical testing shows the API returns errors for descriptions substantially
/// longer than 100 characters. This conservative cap avoids that in practice.
const TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN: usize = 100;

/// Sanitize a skill name into a valid Telegram command name.
/// Telegram commands must be 1-32 characters, lowercase a-z, 0-9, underscore only.
fn sanitize_telegram_command_name(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            result.push(lower);
        } else if !result.ends_with('_') {
            // Replace non-alphanumeric with underscore, collapsing consecutive runs.
            result.push('_');
        }
    }

    let trimmed = result.trim_matches('_');
    if trimmed.len() <= TELEGRAM_COMMAND_NAME_MAX_LEN {
        trimmed.to_string()
    } else {
        trimmed[..TELEGRAM_COMMAND_NAME_MAX_LEN]
            .trim_end_matches('_')
            .to_string()
    }
}

/// Truncate a description to the conservative `TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN` cap.
/// The API nominally supports 256 characters, but empirical testing shows errors occur
/// for descriptions substantially longer than 100 characters.
fn truncate_telegram_command_description(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN {
        return trimmed.to_string();
    }
    let mut truncated: String = trimmed
        .chars()
        .take(TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN - 1)
        .collect();
    truncated.push('…');
    truncated
}

/// Split a message into chunks that respect Telegram's 4096 character limit.
/// Tries to split at word boundaries when possible, and handles continuation.
/// The split budget includes continuation markers and synthetic code fences
/// exactly as `send_text_chunks` will send them.
fn split_message_for_telegram(message: &str) -> Vec<String> {
    if message.chars().count() <= TELEGRAM_MAX_MESSAGE_LENGTH {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;
    let mut in_code_block = false;

    while !remaining.is_empty() {
        let has_previous = !chunks.is_empty();

        if telegram_chunk_send_len(remaining, in_code_block, has_previous, false)
            <= TELEGRAM_MAX_MESSAGE_LENGTH
        {
            let chunk = build_telegram_chunk(remaining, in_code_block, false);
            chunks.push(chunk);
            break;
        }

        let max_take = max_nonfinal_telegram_raw_chars(remaining, in_code_block, has_previous);
        let hard_split = byte_index_after_chars(remaining, max_take);
        let chunk_end = preferred_telegram_split_end(
            remaining,
            hard_split,
            max_take,
            in_code_block,
            has_previous,
        );

        let raw_chunk = &remaining[..chunk_end];
        let starts_in_code_block = in_code_block;
        in_code_block = code_block_state_after(raw_chunk, in_code_block);
        chunks.push(build_telegram_chunk(raw_chunk, starts_in_code_block, true));
        remaining = &remaining[chunk_end..];
    }

    chunks
}

fn build_telegram_chunk(raw_chunk: &str, starts_in_code_block: bool, has_next: bool) -> String {
    let reopen_prefix = if starts_in_code_block {
        TELEGRAM_FENCE_REOPEN
    } else {
        ""
    };
    let ends_in_code_block = code_block_state_after(raw_chunk, starts_in_code_block);
    let needs_synthetic_close = has_next && ends_in_code_block;
    let mut chunk = String::with_capacity(
        reopen_prefix.len()
            + raw_chunk.len()
            + if needs_synthetic_close {
                "\n```".len()
            } else {
                0
            },
    );
    chunk.push_str(reopen_prefix);
    chunk.push_str(raw_chunk);
    if needs_synthetic_close {
        if !chunk.ends_with('\n') {
            chunk.push('\n');
        }
        chunk.push_str(TELEGRAM_FENCE_CLOSE);
    }
    chunk
}

fn format_telegram_text_chunk(chunk: &str, index: usize, total: usize) -> String {
    if total <= 1 {
        return chunk.to_string();
    }

    if index == 0 {
        format!("{chunk}{TELEGRAM_CONTINUES_SUFFIX}")
    } else if index == total - 1 {
        format!("{TELEGRAM_CONTINUED_PREFIX}{chunk}")
    } else {
        format!("{TELEGRAM_CONTINUED_PREFIX}{chunk}{TELEGRAM_CONTINUES_SUFFIX}")
    }
}

fn telegram_chunk_marker_len(has_previous: bool, has_next: bool) -> usize {
    let prefix_len = if has_previous {
        TELEGRAM_CONTINUED_PREFIX.chars().count()
    } else {
        0
    };
    let suffix_len = if has_next {
        TELEGRAM_CONTINUES_SUFFIX.chars().count()
    } else {
        0
    };
    prefix_len + suffix_len
}

fn telegram_chunk_body_len(raw_chunk: &str, starts_in_code_block: bool, has_next: bool) -> usize {
    let reopen_len = if starts_in_code_block {
        TELEGRAM_FENCE_REOPEN.chars().count()
    } else {
        0
    };
    let raw_len = raw_chunk.chars().count();
    let ends_in_code_block = code_block_state_after(raw_chunk, starts_in_code_block);
    let synthetic_close_len = if has_next && ends_in_code_block {
        TELEGRAM_FENCE_CLOSE.chars().count() + usize::from(!raw_chunk.ends_with('\n'))
    } else {
        0
    };

    reopen_len + raw_len + synthetic_close_len
}

fn telegram_chunk_send_len(
    raw_chunk: &str,
    starts_in_code_block: bool,
    has_previous: bool,
    has_next: bool,
) -> usize {
    telegram_chunk_marker_len(has_previous, has_next)
        + telegram_chunk_body_len(raw_chunk, starts_in_code_block, has_next)
}

fn max_nonfinal_telegram_raw_chars(
    remaining: &str,
    starts_in_code_block: bool,
    has_previous: bool,
) -> usize {
    let remaining_chars = remaining.chars().count();
    let marker_len = telegram_chunk_marker_len(has_previous, true);
    let reopen_len = if starts_in_code_block {
        TELEGRAM_FENCE_REOPEN.chars().count()
    } else {
        0
    };
    let upper = remaining_chars
        .saturating_sub(1)
        .min(TELEGRAM_MAX_MESSAGE_LENGTH - marker_len - reopen_len);

    for take in (1..=upper).rev() {
        let end = byte_index_after_chars(remaining, take);
        if telegram_chunk_send_len(&remaining[..end], starts_in_code_block, has_previous, true)
            <= TELEGRAM_MAX_MESSAGE_LENGTH
        {
            return take;
        }
    }

    1
}

fn byte_index_after_chars(s: &str, char_count: usize) -> usize {
    if char_count == 0 {
        return 0;
    }
    s.char_indices()
        .nth(char_count)
        .map_or(s.len(), |(idx, _)| idx)
}

fn preferred_telegram_split_end(
    remaining: &str,
    hard_split: usize,
    max_take: usize,
    starts_in_code_block: bool,
    has_previous: bool,
) -> usize {
    let search_area = &remaining[..hard_split];
    let candidate_fits = |end: usize| {
        end > 0
            && end < remaining.len()
            && telegram_chunk_send_len(&remaining[..end], starts_in_code_block, has_previous, true)
                <= TELEGRAM_MAX_MESSAGE_LENGTH
    };

    if let Some(pos) = search_area.rfind('\n') {
        let end = pos + '\n'.len_utf8();
        if search_area[..pos].chars().count() >= max_take / 2 && candidate_fits(end) {
            return end;
        }
    }

    if let Some(pos) = search_area.rfind(' ') {
        let end = pos + ' '.len_utf8();
        if candidate_fits(end) {
            return end;
        }
    }

    hard_split
}

fn code_block_state_after(text: &str, mut in_code_block: bool) -> bool {
    for line in text.split('\n') {
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
        }
    }
    in_code_block
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

fn telegram_audio_send_spec(
    format: &str,
) -> anyhow::Result<(&'static str, &'static str, &'static str, &'static str)> {
    Ok(match format.trim().to_ascii_lowercase().as_str() {
        "opus" | "ogg" => ("sendVoice", "voice", "voice.ogg", "audio/ogg"),
        "mp3" | "mpeg" => ("sendAudio", "audio", "voice.mp3", "audio/mpeg"),
        "wav" => ("sendAudio", "audio", "voice.wav", "audio/wav"),
        "aac" => ("sendAudio", "audio", "voice.aac", "audio/aac"),
        "flac" => ("sendAudio", "audio", "voice.flac", "audio/flac"),
        // Raw PCM is not a container format; reject so the caller reconfigures
        // the TTS provider to emit a supported container format.
        "pcm" => {
            return Err(anyhow::Error::msg(
                "Telegram does not accept raw PCM audio; \
                 configure the TTS provider to output opus, mp3, wav, aac, or flac",
            ));
        }
        _ => (
            "sendAudio",
            "audio",
            "voice.bin",
            "application/octet-stream",
        ),
    })
}

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

fn safe_attachment_filename(raw: &str) -> String {
    Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("document")
        .to_string()
}

fn media_group_document_storage_filename(
    display_filename: &str,
    chat_id: &str,
    message_id: i64,
) -> String {
    format!("document_{chat_id}_{message_id}_{display_filename}")
}

fn is_http_url(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://")
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

/// Delegate to the shared `strip_tool_call_tags` in the orchestrator module.
fn strip_tool_call_tags(message: &str) -> String {
    crate::orchestrator::strip_tool_call_tags(message)
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
            attachments.push(attachment);
        } else {
            cleaned.push_str(&message[open..=close]);
        }

        cursor = close + 1;
    }

    (cleaned.trim().to_string(), attachments)
}

/// Telegram Bot API maximum file download size (20 MB).
const TELEGRAM_MAX_FILE_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;

/// Default minimum interval between Telegram draft edits.
const TELEGRAM_DRAFT_UPDATE_INTERVAL_MS: u64 = 1000;

/// Telegram channel — long-polls the Bot API for updates
pub struct TelegramChannel {
    bot_token: String,
    /// The alias key under `[channels.telegram.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    persist: Option<Arc<RwLock<Config>>>,
    pairing: Option<PairingGuard>,
    client: reqwest::Client,
    typing_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stream_mode: StreamMode,
    draft_update_interval_ms: u64,
    last_draft_edit: Mutex<std::collections::HashMap<String, std::time::Instant>>,
    mention_only: bool,
    bot_username: Mutex<Option<String>>,
    bot_id: Mutex<Option<i64>>,
    /// Base URL for the Telegram Bot API. Defaults to `https://api.telegram.org`.
    /// Override for local Bot API servers or testing.
    api_base: String,
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    voice_transcriptions: Mutex<std::collections::HashMap<String, String>>,
    workspace_dir: Option<std::path::PathBuf>,
    ack_reactions: bool,
    tts_manager: Option<Arc<super::tts::TtsManager>>,
    voice_chats: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Resolves voice peers from canonical config at call-time.
    /// See AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH" — no cache.
    voice_peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pending_voice:
        Arc<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Pre-computed tool command specs (name, description) for bot command registration.
    tool_command_specs: Vec<(String, String)>,
    /// Pending approval requests: callback_data key → oneshot sender.
    /// `listen()` resolves these when a matching `callback_query` arrives.
    pending_approvals: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                String,
                tokio::sync::oneshot::Sender<zeroclaw_api::channel::ChannelApprovalResponse>,
            >,
        >,
    >,
    /// Seconds to wait for the operator to tap an inline-keyboard button on a
    /// tool approval prompt before auto-denying. Configurable via
    /// `channels.telegram.approval_timeout_secs`. Default: 120.
    approval_timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditMessageResult {
    Success,
    NotModified,
    Failed(reqwest::StatusCode),
}

fn normalize_telegram_api_base(api_base: &str) -> String {
    api_base.trim_end_matches('/').to_string()
}

impl TelegramChannel {
    pub fn new(
        bot_token: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        mention_only: bool,
    ) -> Self {
        let alias = alias.into();
        let has_peers = !peer_resolver().is_empty();
        let pairing = if has_peers {
            None
        } else {
            let guard = PairingGuard::new(true, &[]);
            if let Some(code) = guard.pairing_code() {
                // Surface the one-time bind code through the structured log,
                // not just stdout. A backgrounded daemon (launchd/systemd/
                // GUI-spawned) discards stdout, so the println! alone leaves
                // the operator with no way to retrieve the code. The log
                // lands in runtime-trace.jsonl, the gateway log stream, and
                // `zeroclaw service logs`. Tag it `Channel` (not the default
                // `Internal`) so it survives the web Logs page's default
                // hide-internal filter and is visible without unticking it.
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_attrs(::serde_json::json!({
                            "alias": alias.as_str(),
                            "pairing_code": code.as_str(),
                        })),
                    "Telegram pairing required; one-time bind code issued"
                );
                println!("  🔐 Telegram pairing required. One-time bind code: {code}");
                println!("     Send `{TELEGRAM_BIND_COMMAND} <code>` from your Telegram account.");
            }
            Some(guard)
        };

        Self {
            bot_token,
            alias,
            peer_resolver,
            persist: None,
            pairing,
            client: reqwest::Client::new(),
            stream_mode: StreamMode::Off,
            draft_update_interval_ms: TELEGRAM_DRAFT_UPDATE_INTERVAL_MS,
            last_draft_edit: Mutex::new(std::collections::HashMap::new()),
            typing_handle: Mutex::new(None),
            mention_only,
            bot_username: Mutex::new(None),
            bot_id: Mutex::new(None),
            api_base: TELEGRAM_OFFICIAL_API_BASE_URL.to_string(),
            transcription: None,
            transcription_manager: None,
            voice_transcriptions: Mutex::new(std::collections::HashMap::new()),
            workspace_dir: None,
            ack_reactions: true,
            tts_manager: None,
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            voice_peer_resolver: Arc::new(Vec::new) as Arc<dyn Fn() -> Vec<String> + Send + Sync>,
            pending_voice: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            proxy_url: None,
            tool_command_specs: Vec::new(),
            pending_approvals: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            approval_timeout_secs: 120,
        }
    }

    /// Set the resolver used to resolve voice-chat peers live (no cached state).
    pub fn with_voice_peer_resolver(
        mut self,
        voice_peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        self.voice_peer_resolver = voice_peer_resolver;
        self
    }

    /// Override the approval prompt timeout (default 120s).
    pub fn with_approval_timeout_secs(mut self, secs: u64) -> Self {
        self.approval_timeout_secs = secs;
        self
    }

    /// Configure whether Telegram-native acknowledgement reactions are sent.
    pub fn with_ack_reactions(mut self, enabled: bool) -> Self {
        self.ack_reactions = enabled;
        self
    }

    /// Returns `true` if `recipient` is in a peer group configured with
    /// `output_modality = "voice"` for this channel. Resolved live from config
    /// via `voice_peer_resolver` so it stays correct across hot-reloads.
    pub(crate) fn is_voice_peer(&self, recipient: &str) -> bool {
        (self.voice_peer_resolver)().iter().any(|p| p == recipient)
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Store pre-computed tool command specs for bot command registration.
    pub fn with_tool_command_specs(mut self, specs: Vec<(String, String)>) -> Self {
        self.tool_command_specs = specs;
        self
    }

    /// Configure workspace directory for saving downloaded attachments.
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Configure streaming mode for progressive draft updates.
    pub fn with_streaming(
        mut self,
        stream_mode: StreamMode,
        draft_update_interval_ms: u64,
    ) -> Self {
        self.stream_mode = stream_mode;
        self.draft_update_interval_ms = if draft_update_interval_ms == 0 {
            TELEGRAM_DRAFT_UPDATE_INTERVAL_MS
        } else {
            draft_update_interval_ms
        };
        self
    }

    /// Override the Telegram Bot API base URL.
    /// Useful for local Bot API servers or testing.
    pub fn with_api_base(mut self, api_base: String) -> Self {
        self.api_base = normalize_telegram_api_base(&api_base);
        self
    }

    /// Configure voice transcription.
    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                let names = m.available_providers();
                let m = if names.len() == 1 {
                    let only = names[0].to_string();
                    m.with_agent_transcription_provider(only)
                } else {
                    m
                };
                self.transcription_manager = Some(std::sync::Arc::new(m));
                self.transcription = Some(config);
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "transcription manager init failed, voice transcription disabled"
                );
            }
        }
        self
    }

    pub fn with_typed_transcription_providers(
        mut self,
        typed: &zeroclaw_config::providers::TranscriptionProviders,
        agent_alias: &str,
    ) -> Self {
        if agent_alias.is_empty() || typed.is_empty() {
            return self;
        }
        let base = match self.transcription_manager.take() {
            Some(arc) => match std::sync::Arc::try_unwrap(arc) {
                Ok(m) => m,
                Err(arc) => {
                    self.transcription_manager = Some(arc);
                    return self;
                }
            },
            None => super::transcription::TranscriptionManager::empty(),
        };
        let updated = base
            .with_typed_providers(typed)
            .with_agent_transcription_provider(agent_alias.to_string());
        self.transcription_manager = Some(std::sync::Arc::new(updated));
        self
    }

    /// Set the agent transcription provider alias on the internal TranscriptionManager.
    /// Must be called after `with_transcription`. No-op if transcription was not configured.
    /// The alias should be the provider type key ("groq", "openai", etc.) registered in
    /// the TranscriptionManager, or the full "type.alias" form (the type prefix is extracted).
    pub fn with_agent_transcription_provider(mut self, alias: impl Into<String>) -> Self {
        let alias = alias.into();
        if alias.is_empty() {
            return self;
        }
        // Resolve "groq.default" → "groq" (TranscriptionManager keys by type, not full alias)
        let key = alias.split('.').next().unwrap_or(&alias).to_string();
        if let Some(manager) = self.transcription_manager.take() {
            match std::sync::Arc::try_unwrap(manager) {
                Ok(m) => {
                    self.transcription_manager = Some(std::sync::Arc::new(
                        m.with_agent_transcription_provider(key),
                    ));
                }
                Err(arc) => {
                    self.transcription_manager = Some(arc);
                }
            }
        }
        self
    }

    pub fn with_tts(mut self, config: &zeroclaw_config::schema::Config) -> Self {
        if config.tts.enabled {
            let owner = config.agent_for_channel(&format!("telegram.{}", self.alias));
            match super::tts::TtsManager::from_config_for_agent(config, owner) {
                Ok(m) => self.tts_manager = Some(Arc::new(m)),
                Err(e) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "TTS disabled"
                ),
            }
        }
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

    fn extract_update_message_target(update: &serde_json::Value) -> Option<(String, i64)> {
        let message = update.get("message")?;
        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)?
            .to_string();
        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)?;
        Some((chat_id, message_id))
    }

    fn extract_media_group_key(update: &serde_json::Value) -> Option<MediaGroupKey> {
        let message = update.get("message")?;
        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)?;
        let media_group_id = message
            .get("media_group_id")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        Some((chat_id, media_group_id))
    }

    fn update_id(update: &serde_json::Value) -> Option<i64> {
        update.get("update_id").and_then(serde_json::Value::as_i64)
    }

    fn update_message_id(update: &serde_json::Value) -> Option<i64> {
        update
            .get("message")
            .and_then(|message| message.get("message_id"))
            .and_then(serde_json::Value::as_i64)
    }

    fn is_supported_media_group_update(update: &serde_json::Value) -> bool {
        update
            .get("message")
            .and_then(Self::parse_attachment_metadata)
            .is_some()
    }

    fn should_defer_media_group_update(
        pending: &std::collections::HashMap<MediaGroupKey, PendingMediaGroup>,
        update: &serde_json::Value,
    ) -> bool {
        let Some(key) = Self::extract_media_group_key(update) else {
            return false;
        };
        Self::is_supported_media_group_update(update) || pending.contains_key(&key)
    }

    fn is_duplicate_media_group_member(
        existing: &serde_json::Value,
        candidate: &serde_json::Value,
    ) -> bool {
        let same_update_id = Self::update_id(existing)
            .zip(Self::update_id(candidate))
            .is_some_and(|(existing, candidate)| existing == candidate);
        let same_message_id = Self::update_message_id(existing)
            .zip(Self::update_message_id(candidate))
            .is_some_and(|(existing, candidate)| existing == candidate);
        same_update_id || same_message_id
    }

    fn buffer_media_group_update(
        pending: &mut std::collections::HashMap<MediaGroupKey, PendingMediaGroup>,
        update: &serde_json::Value,
        now: Instant,
        poll_generation: u64,
    ) -> bool {
        let Some(key) = Self::extract_media_group_key(update) else {
            return false;
        };

        // Unsupported members (for example, videos in a photo/video album)
        // are not materialized, but they still belong to the album. When a
        // supported sibling is already pending, treat the unsupported member
        // as activity so the quiet-period flush cannot split later supported
        // siblings into another turn.
        if !Self::is_supported_media_group_update(update) {
            if let Some(group) = pending.get_mut(&key) {
                group.last_seen = now;
                group.last_seen_poll_generation = poll_generation;
            }
            return true;
        }

        let group = pending.entry(key).or_insert_with(|| PendingMediaGroup {
            updates: Vec::new(),
            last_seen: now,
            last_seen_poll_generation: poll_generation,
        });
        if group
            .updates
            .iter()
            .any(|existing| Self::is_duplicate_media_group_member(existing, update))
        {
            return true;
        }

        group.updates.push(update.clone());
        group.last_seen = now;
        group.last_seen_poll_generation = poll_generation;
        true
    }

    fn take_settled_media_groups(
        pending: &mut std::collections::HashMap<MediaGroupKey, PendingMediaGroup>,
        now: Instant,
        completed_poll_generation: u64,
    ) -> Vec<Vec<serde_json::Value>> {
        Self::take_media_groups_matching(pending, |_, group| {
            now.saturating_duration_since(group.last_seen) >= TELEGRAM_MEDIA_GROUP_SETTLE_DELAY
                && group.last_seen_poll_generation < completed_poll_generation
        })
    }

    fn take_prior_media_groups_for_update(
        pending: &mut std::collections::HashMap<MediaGroupKey, PendingMediaGroup>,
        update: &serde_json::Value,
    ) -> Vec<Vec<serde_json::Value>> {
        let Some(message) = update.get("message") else {
            return Vec::new();
        };
        let Some(chat_id) = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
        else {
            return Vec::new();
        };
        let Some(message_id) = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)
        else {
            return Vec::new();
        };
        let Some(update_id) = Self::update_id(update) else {
            return Vec::new();
        };

        Self::take_media_groups_matching(pending, |key, group| {
            key.0 == chat_id
                && !group.updates.is_empty()
                && group.updates.iter().all(|member| {
                    Self::update_id(member).is_some_and(|id| id < update_id)
                        && Self::update_message_id(member).is_some_and(|id| id < message_id)
                })
        })
    }

    fn take_media_groups_matching(
        pending: &mut std::collections::HashMap<MediaGroupKey, PendingMediaGroup>,
        mut should_take: impl FnMut(&MediaGroupKey, &PendingMediaGroup) -> bool,
    ) -> Vec<Vec<serde_json::Value>> {
        let mut matching_keys: Vec<(MediaGroupKey, i64)> = pending
            .iter()
            .filter(|(key, group)| should_take(key, group))
            .map(|(key, group)| {
                let earliest_update_id = group
                    .updates
                    .iter()
                    .filter_map(Self::update_id)
                    .min()
                    .unwrap_or(i64::MAX);
                (key.clone(), earliest_update_id)
            })
            .collect();
        matching_keys.sort_by(|(left_key, left_id), (right_key, right_id)| {
            left_id.cmp(right_id).then_with(|| left_key.cmp(right_key))
        });

        matching_keys
            .into_iter()
            .filter_map(|(key, _)| pending.remove(&key))
            .map(|mut group| {
                group
                    .updates
                    .sort_by_key(|update| Self::update_message_id(update).unwrap_or(i64::MAX));
                group.updates
            })
            .collect()
    }

    fn media_group_poll_timeout_secs(
        pending: &std::collections::HashMap<MediaGroupKey, PendingMediaGroup>,
    ) -> u64 {
        if pending.is_empty() {
            TELEGRAM_IDLE_POLL_TIMEOUT_SECS
        } else {
            TELEGRAM_PENDING_MEDIA_GROUP_POLL_TIMEOUT_SECS
        }
    }

    fn media_group_sender_scope(message: &serde_json::Value) -> Option<String> {
        if let Some(id) = message
            .get("from")
            .and_then(|from| from.get("id"))
            .and_then(serde_json::Value::as_i64)
        {
            return Some(format!("user:{id}"));
        }
        if let Some(id) = message
            .get("sender_chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
        {
            return Some(format!("chat:{id}"));
        }
        message
            .get("from")
            .and_then(|from| from.get("username"))
            .and_then(serde_json::Value::as_str)
            .map(Self::normalize_identity)
            .filter(|username| !username.is_empty())
            .map(|username| format!("username:{username}"))
    }

    fn media_group_members_share_scope(
        anchor: &serde_json::Value,
        candidate: &serde_json::Value,
    ) -> bool {
        let anchor_chat_id = anchor
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64);
        let candidate_chat_id = candidate
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64);
        let anchor_group_id = anchor
            .get("media_group_id")
            .and_then(serde_json::Value::as_str);
        let candidate_group_id = candidate
            .get("media_group_id")
            .and_then(serde_json::Value::as_str);
        let anchor_thread_id = anchor
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64);
        let candidate_thread_id = candidate
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64);

        anchor_chat_id == candidate_chat_id
            && anchor_chat_id.is_some()
            && anchor_group_id == candidate_group_id
            && anchor_group_id.is_some()
            && anchor_thread_id == candidate_thread_id
            && Self::media_group_sender_scope(anchor)
                .zip(Self::media_group_sender_scope(candidate))
                .is_some_and(|(anchor, candidate)| anchor == candidate)
    }

    fn try_add_ack_reaction_nonblocking(&self, chat_id: String, message_id: i64) {
        let client = self.http_client();
        let url = self.api_url("setMessageReaction");
        let emoji = random_telegram_ack_reaction().to_string();
        let body = build_telegram_ack_reaction_request(&chat_id, message_id, &emoji);

        zeroclaw_spawn::spawn!(async move {
            let response = match client.post(&url).json(&body).send().await {
                Ok(resp) => resp,
                Err(err) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"chat_id": chat_id, "message_id": message_id, "err": err.to_string()})), "failed to add ACK reaction to chat_id=, message_id=");
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let err_body = response.text().await.unwrap_or_default();
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"chat_id": chat_id, "message_id": message_id, "status": status.to_string(), "err_body": err_body})), "add ACK reaction failed for chat_id=, message_id=: status=, body=");
            }
        });
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.telegram",
            self.proxy_url.as_deref(),
        )
    }

    fn normalize_identity(value: &str) -> String {
        value.trim().trim_start_matches('@').to_string()
    }

    /// write a paired user into `peer_groups` and save. The long-running
    /// daemon sets this from the orchestrator; tests and one-shot
    /// callers leave it unset (pairing works at runtime, doesn't persist).
    pub fn with_persistence(mut self, config: Arc<RwLock<Config>>) -> Self {
        self.persist = Some(config);
        self
    }

    async fn persist_allowed_identity(&self, identity: &str) -> anyhow::Result<()> {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};

        let Some(config) = &self.persist else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"identity": identity})),
                "paired identity not persisted (no persistence handle wired)"
            );
            return Ok(());
        };
        let normalized = Self::normalize_identity(identity);
        if normalized.is_empty() {
            anyhow::bail!("Cannot persist empty Telegram identity");
        }
        let group_name = format!("telegram_{}", self.alias);
        let channel_ref: zeroclaw_config::providers::ChannelRef =
            format!("telegram.{}", self.alias).into();
        let snapshot = {
            let mut cfg = config.write();
            if !cfg.channels.telegram.contains_key(&self.alias) {
                anyhow::bail!(
                    "Missing [channels.telegram.{}] section. Run `zeroclaw config set channels.telegram.<alias>.bot_token <token>` to configure.",
                    self.alias
                );
            }
            let group = cfg
                .peer_groups
                .entry(group_name)
                .or_insert_with(|| PeerGroupConfig {
                    channel: channel_ref,
                    ..PeerGroupConfig::default()
                });
            if group
                .external_peers
                .iter()
                .any(|p| Self::normalize_identity(p.as_str()) == normalized)
            {
                return Ok(());
            }
            group.external_peers.push(PeerUsername::new(normalized));
            cfg.clone()
        };
        snapshot
            .save()
            .await
            .context("Failed to persist Telegram peer to config.toml")?;
        Ok(())
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

    /// Build the operator-facing `zeroclaw channel bind-telegram` command for
    /// this channel's alias. The CLI defaults to the `default` alias, so only
    /// non-default aliases need the explicit `--alias` flag — emitting it for
    /// the default case would just be noise.
    fn suggested_bind_command(alias: &str, identity: &str) -> String {
        if alias == "default" {
            format!("zeroclaw channel bind-telegram {identity}")
        } else {
            format!("zeroclaw channel bind-telegram {identity} --alias {alias}")
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.api_base, self.bot_token)
    }

    /// Register the bot's slash commands with Telegram via `setMyCommands`.
    /// Called once at startup so that users see a command menu when pressing `/`.
    /// Includes built-in runtime commands, user-installed skill commands, and
    /// enabled tool commands from the configuration.
    async fn register_bot_commands(&self) {
        let mut commands: Vec<serde_json::Value> = vec![
            serde_json::json!({ "command": "new",    "description": "Start a new conversation session" }),
            serde_json::json!({ "command": "clear",  "description": "Clear this conversation session" }),
            serde_json::json!({ "command": "stop",   "description": "Cancel the current in-flight task" }),
            serde_json::json!({ "command": "model",  "description": "Show or switch the current model" }),
            serde_json::json!({ "command": "models", "description": "List available model_providers or switch model_provider" }),
            serde_json::json!({ "command": "config", "description": "Show current configuration" }),
        ];

        // Track registered names to deduplicate across skills and tools.
        let mut used_names: std::collections::HashSet<String> = commands
            .iter()
            .filter_map(|c| c.get("command").and_then(|v| v.as_str()).map(String::from))
            .collect();

        // Collect commands from installed skills.
        if let Some(ref workspace_dir) = self.workspace_dir {
            let skills = zeroclaw_runtime::skills::load_skills(workspace_dir);

            for skill in &skills {
                let sanitized = sanitize_telegram_command_name(&skill.name);
                if sanitized.is_empty() {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "Skipping skill '{}': name produces empty Telegram command",
                            skill.name
                        )
                    );
                    continue;
                }
                if used_names.contains(&sanitized) {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "Skipping skill '{}': command /{sanitized} conflicts with an existing command",
                            skill.name
                        )
                    );
                    continue;
                }
                let description = if skill.description.is_empty() {
                    format!("Run the {name} skill", name = skill.name)
                } else {
                    truncate_telegram_command_description(&skill.description)
                };
                used_names.insert(sanitized.clone());
                commands.push(serde_json::json!({
                    "command": sanitized,
                    "description": description,
                }));
            }
        }

        // Collect commands from enabled tools.
        for (name, description) in &self.tool_command_specs {
            let sanitized = sanitize_telegram_command_name(name);
            if sanitized.is_empty() || used_names.contains(&sanitized) {
                continue;
            }
            used_names.insert(sanitized.clone());
            commands.push(serde_json::json!({
                "command": sanitized,
                "description": truncate_telegram_command_description(description),
            }));
        }

        // Telegram allows at most 100 commands.
        let total_before_cap = commands.len();
        commands.truncate(TELEGRAM_MAX_BOT_COMMANDS);
        if total_before_cap > TELEGRAM_MAX_BOT_COMMANDS {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"TELEGRAM_MAX_BOT_COMMANDS": TELEGRAM_MAX_BOT_COMMANDS, "total_before_cap": total_before_cap})), "Telegram limits bots to commands; configured, registering first . Reduce installed skills to expose more commands.");
        }

        let url = self.api_url("setMyCommands");
        let body = serde_json::json!({ "commands": commands });

        match self.http_client().post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "Telegram bot commands registered successfully ({} commands)",
                        commands.len()
                    )
                );
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"status": status.to_string(), "text": text})
                        ),
                    "Failed to register Telegram bot commands:"
                );
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "Failed to register Telegram bot commands"
                );
            }
        }
    }

    fn is_voice_chat(&self, recipient: &str) -> bool {
        self.voice_chats
            .lock()
            .map(|vs| vs.contains(recipient))
            .unwrap_or(false)
            || (self.voice_peer_resolver)().iter().any(|p| p == recipient)
    }

    fn try_queue_voice_reply(&self, recipient: &str, content: &str, immediate: bool, force: bool) {
        if (!force && !self.is_voice_chat(recipient)) || self.tts_manager.is_none() {
            return;
        }

        // Only queue substantive natural-language replies for voice.
        // Skip tool outputs: URLs, JSON, code blocks, errors, short status.
        let is_substantive = content.len() > 40
            && !content.starts_with("http")
            && !content.starts_with('{')
            && !content.starts_with('[')
            && !content.starts_with("Error")
            && !content.contains("```")
            && !content.contains("tool_call")
            && !content.contains("wttr.in");

        if !is_substantive {
            return;
        }

        let (chat_id, thread_id) = Self::parse_reply_target(recipient);
        let voice_chats = self.voice_chats.clone();
        let voice_peer_resolver = self.voice_peer_resolver.clone();
        let api_base = self.api_base.clone();
        let bot_token = self.bot_token.clone();
        let tts_manager = self.tts_manager.clone().unwrap();

        if immediate {
            // Finalize path: text is already the final answer — no debounce.
            let text = content.to_string();
            let recipient = recipient.to_string();
            zeroclaw_spawn::spawn!(async move {
                let is_config_voice_peer = voice_peer_resolver().contains(&recipient);
                if !is_config_voice_peer && let Ok(mut vc) = voice_chats.lock() {
                    vc.remove(&recipient);
                }
                match Self::synthesize_and_send_voice(
                    &api_base,
                    &bot_token,
                    &chat_id,
                    thread_id.as_deref(),
                    &text,
                    &tts_manager,
                )
                .await
                {
                    Ok(()) => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!("voice reply sent ({} chars)", text.len())
                        );
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                            "TTS voice reply failed"
                        );
                    }
                }
            });
            return;
        }

        // Send path: debounce to coalesce multi-part tool-chain responses.
        if let Ok(mut pv) = self.pending_voice.lock() {
            pv.insert(
                recipient.to_string(),
                (content.to_string(), std::time::Instant::now()),
            );
        }

        let pending = self.pending_voice.clone();
        let recipient = recipient.to_string();
        zeroclaw_spawn::spawn!(async move {
            // Wait 10 seconds — long enough for the agent to finish its
            // full tool chain and send the final answer.
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            // Atomic check-and-remove: only one task gets the value
            let to_voice = pending.lock().ok().and_then(|mut pv| {
                if let Some((_, ts)) = pv.get(&recipient)
                    && ts.elapsed().as_secs() >= 8
                {
                    return pv.remove(&recipient).map(|(text, _)| text);
                }
                None
            });

            if let Some(text) = to_voice {
                let is_config_voice_peer = voice_peer_resolver().contains(&recipient);
                if !is_config_voice_peer && let Ok(mut vc) = voice_chats.lock() {
                    vc.remove(&recipient);
                }
                match Self::synthesize_and_send_voice(
                    &api_base,
                    &bot_token,
                    &chat_id,
                    thread_id.as_deref(),
                    &text,
                    &tts_manager,
                )
                .await
                {
                    Ok(()) => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!("voice reply sent ({} chars)", text.len())
                        );
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                            "TTS voice reply failed"
                        );
                    }
                }
            }
        });
    }

    /// Synthesize text to speech and send as a Telegram voice note (static version for spawned tasks).
    async fn synthesize_and_send_voice(
        api_base: &str,
        bot_token: &str,
        chat_id: &str,
        thread_id: Option<&str>,
        text: &str,
        tts_manager: &crate::tts::TtsManager,
    ) -> anyhow::Result<()> {
        let audio_bytes = tts_manager.synthesize_opus(text).await?;
        let audio_len = audio_bytes.len();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"audio_len": audio_len})),
            "synthesized bytes of audio"
        );

        if audio_bytes.is_empty() {
            anyhow::bail!("TTS returned empty audio");
        }

        // synthesize_opus already transcodes to OGG/Opus via ffmpeg internally
        let (method, field, filename, mime) = telegram_audio_send_spec("opus")?;

        let url = format!("{api_base}/bot{bot_token}/{method}");
        let client = zeroclaw_config::schema::build_runtime_proxy_client("channel.telegram");

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part(
                field,
                reqwest::multipart::Part::bytes(audio_bytes)
                    .file_name(filename)
                    .mime_str(mime)?,
            );

        if let Some(tid) = thread_id {
            form = form.text("message_thread_id", tid.to_string());
        }

        let resp = client.post(&url).multipart(form).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("{method} failed: status={status}, body={body}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"audio_len": audio_len})),
            "sent voice note ( bytes)"
        );
        Ok(())
    }

    async fn classify_edit_message_response(resp: reqwest::Response) -> EditMessageResult {
        if resp.status().is_success() {
            return EditMessageResult::Success;
        }

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if body.contains("message is not modified") {
            return EditMessageResult::NotModified;
        }

        EditMessageResult::Failed(status)
    }

    async fn fetch_bot_username(&self) -> anyhow::Result<String> {
        let resp = self.http_client().get(self.api_url("getMe")).send().await?;

        if !resp.status().is_success() {
            anyhow::bail!("Failed to fetch bot info: {}", resp.status());
        }

        let data: serde_json::Value = resp.json().await?;
        let result = data
            .get("result")
            .context("missing result in getMe response")?;
        let username = result
            .get("username")
            .and_then(|u| u.as_str())
            .context("Bot username not found in response")?;

        // Cache the bot's user ID for reply-to-self detection
        if let Some(id) = result.get("id").and_then(|i| i.as_i64()) {
            let mut cache = self.bot_id.lock();
            *cache = Some(id);
        }

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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "Failed to fetch bot username"
                );
                None
            }
        }
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

    fn normalize_incoming_content(text: &str, _bot_username: &str) -> Option<String> {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    fn is_group_message(message: &serde_json::Value) -> bool {
        message
            .get("chat")
            .and_then(|c| c.get("type"))
            .and_then(|t| t.as_str())
            .map(|t| t == "group" || t == "supergroup")
            .unwrap_or(false)
    }

    /// Check whether `message` is a reply to a message sent by the bot
    /// itself. When true, the `mention_only` gate should be bypassed.
    fn is_reply_to_bot(message: &serde_json::Value, bot_id: i64) -> bool {
        message
            .get("reply_to_message")
            .and_then(|r| r.get("from"))
            .and_then(|f| f.get("id"))
            .and_then(|i| i.as_i64())
            .is_some_and(|id| id == bot_id)
    }

    fn check_media_mention_gate(
        &self,
        message: &serde_json::Value,
        caption: Option<&str>,
    ) -> Option<Option<String>> {
        let is_group = Self::is_group_message(message);
        if !self.mention_only || !is_group {
            return Some(caption.map(String::from));
        }
        let bot_username_guard = self.bot_username.lock();
        let bot_username = bot_username_guard.as_ref()?;

        // If the user is replying directly to the bot's message, bypass the
        // mention check — replies are an unambiguous signal of intent.
        if let Some(caption) = caption
            && let Some(bot_id) = *self.bot_id.lock()
            && Self::is_reply_to_bot(message, bot_id)
        {
            return Some(Self::normalize_incoming_content(caption, bot_username));
        }

        let caption = caption?;
        if !Self::contains_bot_mention(caption, bot_username) {
            return None;
        }
        Some(Self::normalize_incoming_content(caption, bot_username))
    }

    fn is_user_allowed(&self, username: &str) -> bool {
        let identity = Self::normalize_identity(username);
        let peers: Vec<String> = (self.peer_resolver)()
            .into_iter()
            .map(|p| Self::normalize_identity(&p))
            .filter(|p| !p.is_empty())
            .collect();
        crate::allowlist::is_user_allowed(&peers, &identity, crate::allowlist::Match::Sensitive)
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

        let chat_id = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string());

        let Some(chat_id) = chat_id else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "missing chat_id in message, skipping"
            );
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
                            match Box::pin(self.persist_allowed_identity(&identity)).await {
                                Ok(()) => {
                                    let _ = self
                                        .send(&SendMessage::new(
                                            "✅ Telegram account bound successfully. You can talk to ZeroClaw now.",
                                            &chat_id,
                                        ))
                                        .await;
                                    ::zeroclaw_log::record!(
                                        INFO,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_attrs(::serde_json::json!({"identity": identity})),
                                        "paired and allowlisted identity="
                                    );
                                }
                                Err(e) => {
                                    ::zeroclaw_log::record!(
                                        ERROR,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Fail
                                        )
                                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                                        "failed to persist allowlist after bind"
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
                        "ℹ️ Telegram pairing is not active. Ask operator to add your user ID to the matching peer_groups.telegram_<alias>.external_peers entry in config.toml.",
                        &chat_id,
                    ))
                    .await;
            }
            return;
        }

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "ignoring message from unauthorized user: username={username}, sender_id={}. \
Allowlist Telegram username (without '@') or numeric user ID.",
                sender_id_str.as_deref().unwrap_or("unknown")
            )
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

        // Emit the bind command scoped to THIS channel's alias. The CLI
        // handler defaults to the `default` alias, so an alias-less command
        // would silently bind the wrong peer group for a non-default agent
        // and the bot would keep demanding approval.
        let bind_command = Self::suggested_bind_command(&self.alias, &suggested_identity);

        let _ = self
            .send(&SendMessage::new(
                format!(
                    "🔐 This bot requires operator approval.\n\nCopy this command to the operator terminal:\n`{bind_command}`\n\nAfter the operator runs it, send your message again."
                ),
                &chat_id,
            ))
            .await;

        // Only offer the `/bind <code>` path while the channel is genuinely
        // unpaired. Once peers exist (resolved live), the one-time code is
        // moot and the hint just confuses an operator who already authorized
        // someone — the "already assigned but still asks" complaint.
        if self.pairing_code_active() && (self.peer_resolver)().is_empty() {
            let _ = self
                .send(&SendMessage::new(
                    "ℹ️ If the operator provides a one-time pairing code, you can also run `/bind <code>`.",
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

    /// Extract (file_id, duration) from a voice or audio message.
    fn parse_voice_metadata(message: &serde_json::Value) -> Option<(String, u64)> {
        let voice = message.get("voice").or_else(|| message.get("audio"))?;
        let file_id = voice.get("file_id")?.as_str()?.to_string();
        let duration = voice
            .get("duration")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        Some((file_id, duration))
    }

    /// Extract attachment metadata from an incoming Telegram message (document or photo).
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

    fn allowed_attachment_sender(&self, message: &serde_json::Value) -> Option<String> {
        let (username, sender_id, sender_identity) = Self::extract_sender_info(message);
        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }
        self.is_any_user_allowed(identities.iter().copied())
            .then_some(sender_identity)
    }

    /// Download and persist one attachment, returning only its prompt marker.
    /// Authorization, mention gating, captions, replies, and forwarding are
    /// intentionally handled by the caller so an album applies them once.
    async fn materialize_attachment_content(
        &self,
        attachment: &IncomingAttachment,
        chat_id: &str,
        message_id: i64,
        disambiguate_document_name: bool,
    ) -> Option<String> {
        if let Some(size) = attachment.file_size
            && size > TELEGRAM_MAX_FILE_DOWNLOAD_BYTES
        {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Skipping attachment: file size {size} bytes exceeds {} MB limit",
                    TELEGRAM_MAX_FILE_DOWNLOAD_BYTES / (1024 * 1024)
                )
            );
            return None;
        }

        // Ensure workspace directory is configured
        let workspace = self.workspace_dir.as_ref().or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Cannot save attachment: workspace_dir not configured"
            );
            None
        })?;

        let save_dir = workspace.join("telegram_files");
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                "Failed to create telegram_files directory"
            );
            return None;
        }

        // Download file from Telegram
        let tg_file_path = match self.get_file_path(&attachment.file_id).await {
            Ok(p) => p,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "Failed to get attachment file path"
                );
                return None;
            }
        };

        let file_data = match self.download_file(&tg_file_path).await {
            Ok(d) => d,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "Failed to download attachment"
                );
                return None;
            }
        };

        // Determine local filename
        let (local_filename, display_filename) = match &attachment.file_name {
            Some(name) => {
                let display_filename = safe_attachment_filename(name);
                let local_filename = if disambiguate_document_name
                    && attachment.kind == IncomingAttachmentKind::Document
                {
                    media_group_document_storage_filename(&display_filename, chat_id, message_id)
                } else {
                    display_filename.clone()
                };
                (local_filename, display_filename)
            }
            None => {
                // For photos, derive extension from Telegram file path
                let ext = tg_file_path.rsplit('.').next().unwrap_or("jpg");
                let filename = format!("photo_{chat_id}_{message_id}.{ext}");
                (filename.clone(), filename)
            }
        };

        let local_path = save_dir.join(&local_filename);
        if let Err(e) = tokio::fs::write(&local_path, &file_data).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                &format!("Failed to save attachment to {}", local_path.display())
            );
            return None;
        }

        // Build message content.
        // Photos with image extensions use [IMAGE:] marker so the multimodal
        // pipeline validates vision capability. Non-image files always get
        // [Document:] format regardless of Telegram's classification.
        Some(format_attachment_content(
            attachment.kind,
            &display_filename,
            &local_path,
        ))
    }

    fn finalize_attachment_message(
        &self,
        message: &serde_json::Value,
        sender_identity: String,
        mut content: String,
        gated_caption: Option<&str>,
    ) -> Option<ChannelMessage> {
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
            format!("{chat_id}:{tid}")
        } else {
            chat_id.clone()
        };

        if let Some(caption) = gated_caption
            && !caption.is_empty()
        {
            use std::fmt::Write;
            let _ = write!(content, "\n\n{caption}");
        }

        // Prepend reply context if replying to another message
        if let Some(quote) = self.extract_reply_context(message) {
            content = format!("{quote}\n\n{content}");
        }

        // Prepend forwarding attribution when the message was forwarded
        if let Some(attr) = Self::format_forward_attribution(message) {
            content = Self::prepend_forward_attribution(&attr, content);
        }

        Some(ChannelMessage {
            id: format!("telegram_{chat_id}_{message_id}"),
            sender: sender_identity,
            reply_target,
            content,
            channel: "telegram".into(),
            channel_alias: Some(self.alias.clone()),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: thread_id,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
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
        let sender_identity = self.allowed_attachment_sender(message)?;

        // Apply mention_only gate before downloading. Photo / document
        // updates carry no `text` field, so the text-only gate in
        // `parse_update_message` can never see them and they used to slip
        // through unconditionally.
        let gated_caption =
            self.check_media_mention_gate(message, attachment.caption.as_deref())?;
        let (chat_id, message_id) = Self::extract_update_message_target(update)?;
        let content = self
            .materialize_attachment_content(&attachment, &chat_id, message_id, false)
            .await?;
        self.finalize_attachment_message(
            message,
            sender_identity,
            content,
            gated_caption.as_deref(),
        )
    }

    /// Materialize one settled Telegram media group as one inbound message.
    /// Group scope, authorization, and mention gating are validated before
    /// any file download. Individual attachment failures do not discard
    /// successfully materialized siblings.
    async fn try_parse_media_group(&self, updates: &[serde_json::Value]) -> Option<ChannelMessage> {
        let mut ordered: Vec<&serde_json::Value> = updates.iter().collect();
        ordered.sort_by_key(|update| Self::update_message_id(update).unwrap_or(i64::MAX));

        let anchor_update = *ordered.first()?;
        let anchor_message = anchor_update.get("message")?;
        if !ordered.iter().all(|update| {
            update.get("message").is_some_and(|message| {
                Self::media_group_members_share_scope(anchor_message, message)
            })
        }) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Rejecting Telegram media group with mixed chat, sender, or thread scope"
            );
            return None;
        }

        let sender_identity = self.allowed_attachment_sender(anchor_message)?;
        let mut seen_captions = std::collections::HashSet::new();
        let shared_caption = ordered
            .iter()
            .filter_map(|update| {
                update
                    .get("message")
                    .and_then(|message| message.get("caption"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|caption| !caption.trim().is_empty())
            })
            .filter(|caption| seen_captions.insert((*caption).to_string()))
            .collect::<Vec<_>>()
            .join("\n\n");
        let gated_caption = self.check_media_mention_gate(
            anchor_message,
            (!shared_caption.is_empty()).then_some(shared_caption.as_str()),
        )?;

        let mut contents = Vec::with_capacity(ordered.len());
        for update in ordered {
            let Some(message) = update.get("message") else {
                continue;
            };
            let Some(attachment) = Self::parse_attachment_metadata(message) else {
                continue;
            };
            let Some((chat_id, message_id)) = Self::extract_update_message_target(update) else {
                continue;
            };
            if let Some(content) = self
                .materialize_attachment_content(&attachment, &chat_id, message_id, true)
                .await
            {
                contents.push(content);
            }
        }

        if contents.is_empty() {
            return None;
        }

        self.finalize_attachment_message(
            anchor_message,
            sender_identity,
            contents.join("\n\n"),
            gated_caption.as_deref(),
        )
    }

    /// Attempt to parse a Telegram update as a voice message and transcribe it.
    /// Returns `None` if the message is not a voice message, transcription is disabled,
    /// or the message exceeds duration limits.
    async fn try_parse_voice_message(&self, update: &serde_json::Value) -> Option<ChannelMessage> {
        let config = self.transcription.as_ref()?;
        let manager = self.transcription_manager.as_deref()?;
        let message = update.get("message")?;

        let (file_id, duration) = Self::parse_voice_metadata(message)?;

        if duration > config.max_duration_secs {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Skipping voice message: duration {duration}s exceeds limit {}s",
                    config.max_duration_secs
                )
            );
            return None;
        }

        let (username, sender_id, sender_identity) = Self::extract_sender_info(message);

        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }

        if !self.is_any_user_allowed(identities.iter().copied()) {
            return None;
        }

        let voice_caption = message.get("caption").and_then(serde_json::Value::as_str);
        self.check_media_mention_gate(message, voice_caption)?;

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
        let file_path = match self.get_file_path(&file_id).await {
            Ok(p) => p,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "Failed to get voice file path"
                );
                return None;
            }
        };

        let file_name = file_path
            .rsplit('/')
            .next()
            .unwrap_or("voice.ogg")
            .to_string();

        let audio_data = match self.download_file(&file_path).await {
            Ok(d) => d,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "Failed to download voice file"
                );
                return None;
            }
        };

        let text = match manager.transcribe(&audio_data, &file_name).await {
            Ok(t) => t,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "Voice transcription failed"
                );
                return None;
            }
        };

        if text.trim().is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Voice transcription returned empty text, skipping"
            );
            return None;
        }

        // Enter voice-chat mode so outgoing replies get a TTS voice note
        if let Ok(mut vc) = self.voice_chats.lock() {
            vc.insert(reply_target.clone());
        }

        // Cache transcription for reply-context lookups
        {
            let mut cache = self.voice_transcriptions.lock();
            if cache.len() >= 100 {
                cache.clear();
            }
            cache.insert(format!("{chat_id}:{message_id}"), text.clone());
        }

        let content = if let Some(quote) = self.extract_reply_context(message) {
            format!("{quote}\n\n[Voice] {text}")
        } else {
            format!("[Voice] {text}")
        };

        // Prepend forwarding attribution when the message was forwarded
        let content = if let Some(attr) = Self::format_forward_attribution(message) {
            Self::prepend_forward_attribution(&attr, content)
        } else {
            content
        };

        Some(ChannelMessage {
            id: format!("telegram_{chat_id}_{message_id}"),
            sender: sender_identity,
            reply_target,
            content,
            channel: "telegram".into(),
            channel_alias: Some(self.alias.clone()),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: thread_id,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
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

    /// Build a forwarding attribution prefix from Telegram forward fields.
    /// Returns `Some("[Forwarded from ...] ")` when the message is forwarded,
    /// `None` otherwise.
    fn format_forward_attribution(message: &serde_json::Value) -> Option<String> {
        if let Some(origin) = message.get("forward_origin") {
            let origin_type = origin.get("type").and_then(serde_json::Value::as_str)?;
            let label = match origin_type {
                "user" => {
                    let sender = origin.get("sender_user")?;
                    Self::format_forwarded_user_label(sender, "unknown")
                }
                "hidden_user" => origin
                    .get("sender_user_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown hidden user")
                    .to_string(),
                "chat" => {
                    let title = origin
                        .get("sender_chat")
                        .and_then(|chat| chat.get("title"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown chat");
                    format!("chat: {title}")
                }
                "channel" => {
                    let title = origin
                        .get("chat")
                        .and_then(|chat| chat.get("title"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown channel");
                    format!("channel: {title}")
                }
                _ => "unknown source".to_string(),
            };
            Some(format!("[Forwarded from {label}] "))
        } else if let Some(from_chat) = message.get("forward_from_chat") {
            // Forwarded from a channel or group
            let title = from_chat
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown channel");
            Some(format!("[Forwarded from channel: {title}] "))
        } else if let Some(from_user) = message.get("forward_from") {
            // Forwarded from a user (privacy allows identity)
            let label = Self::format_forwarded_user_label(from_user, "unknown");
            Some(format!("[Forwarded from {label}] "))
        } else {
            // Forwarded from a user who hides their identity
            message
                .get("forward_sender_name")
                .and_then(serde_json::Value::as_str)
                .map(|name| format!("[Forwarded from {name}] "))
        }
    }

    fn prepend_forward_attribution(attr: &str, content: String) -> String {
        let attr = attr.trim_end();
        if content.starts_with("> ") {
            format!("{attr}\n\n{content}")
        } else {
            format!("{attr} {content}")
        }
    }

    fn format_forwarded_user_label(user: &serde_json::Value, fallback: &str) -> String {
        if let Some(username) = user.get("username").and_then(serde_json::Value::as_str) {
            return format!("@{username}");
        }

        let Some(first_name) = user.get("first_name").and_then(serde_json::Value::as_str) else {
            return fallback.to_string();
        };

        let mut label = first_name.to_string();
        if let Some(last_name) = user.get("last_name").and_then(serde_json::Value::as_str) {
            label.push(' ');
            label.push_str(last_name);
        }
        label
    }

    /// Extract reply context from a Telegram `reply_to_message`, if present.
    fn extract_reply_context(&self, message: &serde_json::Value) -> Option<String> {
        let reply = message.get("reply_to_message")?;

        let reply_mid = reply.get("message_id").and_then(serde_json::Value::as_i64);
        let thread_id = message
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64);
        if let (Some(rmid), Some(tid)) = (reply_mid, thread_id)
            && rmid == tid
        {
            return None;
        }

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
        if self.mention_only && is_group {
            let bot_username = self.bot_username.lock();
            let bot_username = bot_username.as_ref()?;
            // If the user is replying directly to the bot's message, bypass
            // the mention check — replies are an unambiguous signal of intent.
            if !Self::contains_bot_mention(text, bot_username) {
                let bot_id = *self.bot_id.lock();
                if bot_id.is_none_or(|id| !Self::is_reply_to_bot(message, id)) {
                    return None;
                }
            }
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

        let content = if self.mention_only && is_group {
            let bot_username = self.bot_username.lock();
            let bot_username = bot_username.as_ref()?;
            Self::normalize_incoming_content(text, bot_username)?
        } else {
            text.to_string()
        };

        let content = if let Some(quote) = self.extract_reply_context(message) {
            format!("{quote}\n\n{content}")
        } else {
            content
        };

        // Prepend forwarding attribution when the message was forwarded
        let content = if let Some(attr) = Self::format_forward_attribution(message) {
            Self::prepend_forward_attribution(&attr, content)
        } else {
            content
        };

        // Exit input-driven voice mode when user switches back to typing.
        // Config-mandated voice peers (output_modality = "voice") stay in
        // voice mode regardless of whether they send text or voice.
        if !self.is_voice_peer(&reply_target)
            && let Ok(mut vc) = self.voice_chats.lock()
        {
            vc.remove(&reply_target);
        }

        Some(ChannelMessage {
            id: format!("telegram_{chat_id}_{message_id}"),
            sender: sender_identity,
            reply_target,
            content,
            channel: "telegram".into(),
            channel_alias: Some(self.alias.clone()),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: thread_id,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
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
                if i + 1 < len
                    && bytes[i] == b'*'
                    && bytes[i + 1] == b'*'
                    && let Some(end) = line[i + 2..].find("**")
                {
                    let inner = Self::escape_html(&line[i + 2..i + 2 + end]);
                    let _ = write!(line_out, "<b>{inner}</b>");
                    i += 4 + end;
                    continue;
                }
                if i + 1 < len
                    && bytes[i] == b'_'
                    && bytes[i + 1] == b'_'
                    && let Some(end) = line[i + 2..].find("__")
                {
                    let inner = Self::escape_html(&line[i + 2..i + 2 + end]);
                    let _ = write!(line_out, "<b>{inner}</b>");
                    i += 4 + end;
                    continue;
                }
                // Italic: *text* or _text_ (single)
                if bytes[i] == b'*'
                    && (i == 0 || bytes[i - 1] != b'*')
                    && let Some(end) = line[i + 1..].find('*')
                    && end > 0
                {
                    let inner = Self::escape_html(&line[i + 1..i + 1 + end]);
                    let _ = write!(line_out, "<i>{inner}</i>");
                    i += 2 + end;
                    continue;
                }
                // Inline code: `code`
                if bytes[i] == b'`'
                    && (i == 0 || bytes[i - 1] != b'`')
                    && let Some(end) = line[i + 1..].find('`')
                {
                    let inner = Self::escape_html(&line[i + 1..i + 1 + end]);
                    let _ = write!(line_out, "<code>{inner}</code>");
                    i += 2 + end;
                    continue;
                }
                // Markdown link: [text](url)
                if bytes[i] == b'['
                    && let Some(bracket_end) = line[i + 1..].find(']')
                {
                    let text_part = &line[i + 1..i + 1 + bracket_end];
                    let after_bracket = i + 1 + bracket_end + 1; // position after ']'
                    if after_bracket < len
                        && bytes[after_bracket] == b'('
                        && let Some(paren_end) = line[after_bracket + 1..].find(')')
                    {
                        let url = &line[after_bracket + 1..after_bracket + 1 + paren_end];
                        if url.starts_with("http://") || url.starts_with("https://") {
                            let text_html = Self::escape_html(text_part);
                            let url_html = Self::escape_html(url);
                            let _ = write!(line_out, "<a href=\"{url_html}\">{text_html}</a>");
                            i = after_bracket + 1 + paren_end + 1;
                            continue;
                        }
                    }
                }
                // Strikethrough: ~~text~~
                if i + 1 < len
                    && bytes[i] == b'~'
                    && bytes[i + 1] == b'~'
                    && let Some(end) = line[i + 2..].find("~~")
                {
                    let inner = Self::escape_html(&line[i + 2..i + 2 + end]);
                    let _ = write!(line_out, "<s>{inner}</s>");
                    i += 4 + end;
                    continue;
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
                    let _ = writeln!(final_out, "<pre><code>{escaped}</code></pre>");
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
            let _ = writeln!(final_out, "<pre><code>{}</code></pre>", code_buf.trim_end());
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
            let text = format_telegram_text_chunk(chunk, index, chunks.len());

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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"status": markdown_status.to_string()})),
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
                anyhow::bail!(
                    "Telegram sendMessage failed (markdown {}: {}; plain {}: {})",
                    markdown_status,
                    markdown_err,
                    plain_status,
                    plain_err
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
            anyhow::bail!("{method} by URL failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({"method": method, "chat_id": chat_id, "url": url})
            ),
            "sent to"
        );
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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"url": target, "error": zeroclaw_runtime::security::scrub(&format!("{}", e))})
                        ),
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

        // Remap Docker container workspace path (/workspace/...) to the host
        // workspace directory so files written by the containerised runtime
        // can be found and sent by the host-side Telegram sender.
        let remapped;
        let target = if let Some(rel) = target.strip_prefix("/workspace/") {
            if let Some(ws) = &self.workspace_dir {
                remapped = ws.join(rel);
                remapped.to_str().unwrap_or(target)
            } else {
                target
            }
        } else {
            target
        };

        let path = Path::new(target);
        if !path.exists() {
            anyhow::bail!("Telegram attachment path not found: {target}");
        }

        match attachment.kind {
            TelegramAttachmentKind::Image => self.send_photo(chat_id, thread_id, path, None).await,
            TelegramAttachmentKind::Document => {
                self.send_document(chat_id, thread_id, path, None).await
            }
            TelegramAttachmentKind::Video => self.send_video(chat_id, thread_id, path, None).await,
            TelegramAttachmentKind::Audio => self.send_audio(chat_id, thread_id, path, None).await,
            TelegramAttachmentKind::Voice => self.send_voice(chat_id, thread_id, path, None).await,
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
            anyhow::bail!("Telegram sendDocument failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "file_name": file_name})),
            "document sent to"
        );
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
            anyhow::bail!("Telegram sendDocument failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "file_name": file_name})),
            "document sent to"
        );
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
            anyhow::bail!("Telegram sendPhoto failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "file_name": file_name})),
            "photo sent to"
        );
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
            anyhow::bail!("Telegram sendPhoto failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "file_name": file_name})),
            "photo sent to"
        );
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
            anyhow::bail!("Telegram sendVideo failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "file_name": file_name})),
            "video sent to"
        );
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
            anyhow::bail!("Telegram sendAudio failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "file_name": file_name})),
            "audio sent to"
        );
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
            anyhow::bail!("Telegram sendVoice failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "file_name": file_name})),
            "voice sent to"
        );
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
            anyhow::bail!("Telegram sendDocument by URL failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "url": url})),
            "document (URL) sent to"
        );
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
            anyhow::bail!("Telegram sendPhoto by URL failed: {err}");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"chat_id": chat_id, "url": url})),
            "photo (URL) sent to"
        );
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

impl ::zeroclaw_api::attribution::Attributable for TelegramChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Telegram,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl TelegramChannel {
    async fn dispatch_incoming_message(
        &self,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        update: &serde_json::Value,
        msg: ChannelMessage,
    ) -> bool {
        if self.ack_reactions
            && let Some((reaction_chat_id, reaction_message_id)) =
                Self::extract_update_message_target(update)
        {
            self.try_add_ack_reaction_nonblocking(reaction_chat_id, reaction_message_id);
        }

        // Send one typing indicator for the logical inbound message. A media
        // group reaches this helper only after all members are materialized.
        let typing_body = serde_json::json!({
            "chat_id": &msg.reply_target,
            "action": "typing"
        });
        let _ = self
            .http_client()
            .post(self.api_url("sendChatAction"))
            .json(&typing_body)
            .send()
            .await;

        tx.send(msg).await.is_ok()
    }

    async fn flush_settled_media_groups(
        &self,
        pending: &mut std::collections::HashMap<MediaGroupKey, PendingMediaGroup>,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        now: Instant,
        completed_poll_generation: u64,
    ) -> bool {
        let batches = Self::take_settled_media_groups(pending, now, completed_poll_generation);
        self.dispatch_media_group_batches(tx, batches).await
    }

    async fn dispatch_media_group_batches(
        &self,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        batches: Vec<Vec<serde_json::Value>>,
    ) -> bool {
        for updates in batches {
            let Some(anchor_update) = updates.first() else {
                continue;
            };
            let Some(msg) = self.try_parse_media_group(&updates).await else {
                Box::pin(self.handle_unauthorized_message(anchor_update)).await;
                continue;
            };
            if !self.dispatch_incoming_message(tx, anchor_update, msg).await {
                return false;
            }
        }
        true
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    fn self_handle(&self) -> Option<String> {
        self.bot_username.lock().clone()
    }

    /// Telegram users mention the bot as `@bot_username` in chat. The
    /// cached `bot_username` from `getMe` is already the bare form;
    /// prepend `@` to match what arrives in inbound message text.
    fn self_addressed_mention(&self) -> Option<String> {
        self.self_handle().map(|name| {
            let trimmed = name.trim_start_matches('@');
            format!("@{trimmed}")
        })
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
            anyhow::bail!("Telegram sendMessage (draft) failed: {err}");
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
    ) -> anyhow::Result<()> {
        let (chat_id, _) = Self::parse_reply_target(recipient);

        // Rate-limit edits per chat
        {
            let last_edits = self.last_draft_edit.lock();
            if let Some(last_time) = last_edits.get(&chat_id) {
                let elapsed = u64::try_from(last_time.elapsed().as_millis()).unwrap_or(u64::MAX);
                if elapsed < self.draft_update_interval_ms {
                    return Ok(());
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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e)), "message_id": message_id})
                        ),
                    "Invalid Telegram message_id ''"
                );
                return Ok(());
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
            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"error": format!("{}", err), "status": status.to_string()})), "editMessageText failed");
        }

        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        suppress_voice: bool,
    ) -> anyhow::Result<()> {
        let text = &strip_tool_call_tags(text);
        let (chat_id, thread_id) = Self::parse_reply_target(recipient);

        // Queue TTS voice reply — immediate mode since text is already final.
        // Skipped when suppress_voice is set (explicit text-only routing override).
        if !suppress_voice {
            self.try_queue_voice_reply(recipient, text, true, false);
        }

        // Clean up rate-limit tracking for this chat
        self.last_draft_edit.lock().remove(&chat_id);

        // Voice-only peers: delete the draft placeholder and let the voice
        // bubble be the sole reply. Bypassed when suppress_voice forces text.
        if !suppress_voice && self.is_voice_peer(recipient) {
            if let Ok(id) = message_id.parse::<i64>() {
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
            return Ok(());
        }

        // Parse attachments before processing
        let (text_without_markers, attachments) = parse_attachment_markers(text);

        // Parse message ID once for reuse
        let msg_id = match message_id.parse::<i64>() {
            Ok(id) => Some(id),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e)), "message_id": message_id})
                        ),
                    "Invalid Telegram message_id ''"
                );
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

        match Self::classify_edit_message_response(resp).await {
            EditMessageResult::Success | EditMessageResult::NotModified => return Ok(()),
            EditMessageResult::Failed(status) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"status": status.to_string()})),
                    "Telegram finalize_draft HTML edit failed; retrying without parse_mode"
                );
            }
        }

        // HTML failed — retry without parse_mode
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

        match Self::classify_edit_message_response(resp).await {
            EditMessageResult::Success | EditMessageResult::NotModified => return Ok(()),
            EditMessageResult::Failed(status) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"status": status.to_string()})),
                    "Telegram finalize_draft plain edit failed; attempting delete+send fallback"
                );
            }
        }

        let delete_resp = self
            .client
            .post(self.api_url("deleteMessage"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "message_id": id,
            }))
            .send()
            .await;

        match delete_resp {
            Ok(resp) if resp.status().is_success() => {
                self.send_text_chunks(text, &chat_id, thread_id.as_deref())
                    .await
            }
            Ok(resp) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"status": resp.status().to_string()})),
                    "Telegram finalize_draft delete failed; skipping sendMessage to avoid duplicate"
                );
                Ok(())
            }
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"err": err.to_string()})),
                    "Telegram finalize_draft delete request failed: ; skipping sendMessage to avoid duplicate"
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
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(
                            ::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e)), "message_id": message_id})
                        ),
                    "Invalid Telegram draft message_id ''"
                );
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
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"status": status.to_string(), "body": body})),
                "deleteMessage failed"
            );
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

        // Voice chat mode: queue a voice note. Suppressed messages (errors,
        // system notices) are never voiced.
        if !message.suppress_voice {
            self.try_queue_voice_reply(&message.recipient, &content, false, message.force_voice);
        }

        // Voice-only peers (or explicit force_voice): the voice note is the sole reply — skip text.
        if !message.suppress_voice
            && (self.is_voice_peer(&message.recipient) || message.force_voice)
        {
            return Ok(());
        }

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

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let mut offset: i64 = 0;
        let mut poll_generation: u64 = 0;
        let mut pending_media_groups: std::collections::HashMap<MediaGroupKey, PendingMediaGroup> =
            std::collections::HashMap::new();

        if self.mention_only {
            let _ = self.get_bot_username().await;
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "channel listening for messages..."
        );

        loop {
            let url = self.api_url("getUpdates");
            let probe = serde_json::json!({
                "offset": offset,
                "timeout": 0,
                "allowed_updates": ["message", "callback_query"]
            });
            match self.http_client().post(&url).json(&probe).send().await {
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})
                            ),
                        "startup probe error; retrying in 5s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                Ok(resp) => {
                    match resp.json::<serde_json::Value>().await {
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"e": e.to_string()})),
                                "startup probe parse error: ; retrying in 5s"
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
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    ),
                                    "Startup probe: slot busy (409), retrying in 5s"
                                );
                            } else {
                                let desc = data
                                    .get("description")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("unknown");
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error_code": error_code, "desc": desc})), "Startup probe: API error : ; retrying in 5s");
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            }
        }

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Startup probe succeeded; entering main long-poll loop."
        );

        self.register_bot_commands().await;

        loop {
            if self.mention_only {
                let missing_username = self.bot_username.lock().is_none();
                if missing_username {
                    let _ = self.get_bot_username().await;
                }
            }

            let url = self.api_url("getUpdates");
            let poll_timeout_secs = Self::media_group_poll_timeout_secs(&pending_media_groups);
            let body = serde_json::json!({
                "offset": offset,
                "timeout": poll_timeout_secs,
                "allowed_updates": ["message", "callback_query"]
            });

            let resp = match self.http_client().post(&url).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})
                        ),
                        "poll error"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            let data: serde_json::Value = match resp.json().await {
                Ok(d) => d,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})
                        ),
                        "parse error"
                    );
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
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"description": description})),
                        "Telegram polling conflict (409): . \
Ensure only one `zeroclaw` process is using this bot token."
                    );
                    // Back off for 35 seconds — longer than Telegram's 30-second poll
                    // timeout — so any competing session (e.g. a stale connection from
                    // a previous daemon) has time to expire before we retry.
                    tokio::time::sleep(std::time::Duration::from_secs(35)).await;
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "Telegram getUpdates API error (code={}): {description}",
                            error_code
                        )
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                continue;
            }

            poll_generation = poll_generation.saturating_add(1);
            // Debounce against the time this response was observed, not the
            // time its updates finish processing. Slow downloads or channel
            // backpressure must not make an album look quiet prematurely.
            let poll_completed_at = Instant::now();

            if let Some(results) = data.get("result").and_then(serde_json::Value::as_array) {
                // Buffer every supported album member in this response before
                // dispatching any ordinary update. This lets a later same-chat
                // message act as an ordering boundary without depending on the
                // array's internal interleaving.
                for update in results {
                    Self::buffer_media_group_update(
                        &mut pending_media_groups,
                        update,
                        poll_completed_at,
                        poll_generation,
                    );
                }

                for update in results {
                    // Advance offset past this update
                    if let Some(uid) = update.get("update_id").and_then(serde_json::Value::as_i64) {
                        offset = uid + 1;
                    }

                    // ── Handle callback_query (inline keyboard taps) ──
                    if let Some(cb) = update.get("callback_query") {
                        let cb_id = cb
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let cb_data = cb
                            .get("data")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();

                        if let Some(rest) = cb_data.strip_prefix("approval:")
                            && let Some((approval_id, action)) = rest.rsplit_once(':')
                        {
                            let response = match action {
                                "approve" => {
                                    Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve)
                                }
                                "always" => Some(
                                    zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove,
                                ),
                                "deny" => {
                                    Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny)
                                }
                                other => {
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                        .with_attrs(::serde_json::json!({"other": other})),
                                        "Unknown approval callback action"
                                    );
                                    None
                                }
                            };

                            if let Some(resp) = response
                                && let Some(sender) =
                                    self.pending_approvals.lock().await.remove(approval_id)
                            {
                                let _ = sender.send(resp);
                            }

                            // Answer the callback query to dismiss the spinner.
                            let answer_text = match action {
                                "approve" => "✅ Approved",
                                "always" => "✅✅ Always approved",
                                "deny" => "❌ Denied",
                                _ => "⚠️ Unknown action",
                            };
                            let answer_body = serde_json::json!({
                                "callback_query_id": cb_id,
                                "text": answer_text,
                            });
                            if let Err(e) = self
                                .http_client()
                                .post(self.api_url("answerCallbackQuery"))
                                .json(&answer_body)
                                .send()
                                .await
                            {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                                    "answerCallbackQuery failed"
                                );
                            }
                        }

                        continue; // callback_query is not a regular message
                    }

                    if Self::should_defer_media_group_update(&pending_media_groups, update) {
                        continue;
                    }

                    let prior_groups =
                        Self::take_prior_media_groups_for_update(&mut pending_media_groups, update);
                    if !self.dispatch_media_group_batches(&tx, prior_groups).await {
                        return Ok(());
                    }

                    let msg = if let Some(m) = self.parse_update_message(update) {
                        m
                    } else if let Some(m) = self.try_parse_voice_message(update).await {
                        m
                    } else if let Some(m) = self.try_parse_attachment_message(update).await {
                        m
                    } else {
                        Box::pin(self.handle_unauthorized_message(update)).await;
                        continue;
                    };

                    if !self.dispatch_incoming_message(&tx, update, msg).await {
                        return Ok(());
                    }
                }
            }

            if !self
                .flush_settled_media_groups(
                    &mut pending_media_groups,
                    &tx,
                    poll_completed_at,
                    poll_generation,
                )
                .await
            {
                return Ok(());
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
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                    "health check failed"
                );
                false
            }
            Err(_) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "health check timed out after 5s"
                );
                false
            }
        }
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.stop_typing(recipient).await?;

        let client = self.http_client();
        let url = self.api_url("sendChatAction");
        let chat_id = recipient.to_string();

        let handle = zeroclaw_spawn::spawn!(async move {
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

    async fn request_approval(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        // Parse recipient for chat_id + optional thread_id ("chat_id:thread_id" format).
        let (chat_id, thread_id) = recipient
            .split_once(':')
            .map_or((recipient, None), |(c, t)| (c, Some(t)));

        // Unique key embedded in callback_data so listen() can route the tap.
        let approval_id = uuid::Uuid::new_v4().to_string();

        let tool = Self::escape_html(&request.tool_name);
        let args = Self::escape_html(&request.arguments_summary);
        let text = format!(
            "\u{1f527} <b>Tool approval required</b>\n\n\
             Tool: <code>{tool}</code>\n\
             {args}\n\n\
             Tap a button below:",
        );

        let reply_markup = serde_json::json!({
            "inline_keyboard": [[
                { "text": "✅ Approve",  "callback_data": format!("approval:{}:approve", approval_id) },
                { "text": "❌ Deny",     "callback_data": format!("approval:{}:deny", approval_id) },
                { "text": "✅✅ Always", "callback_data": format!("approval:{}:always", approval_id) },
            ]]
        });

        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML",
            "reply_markup": reply_markup,
        });
        if let Some(tid) = thread_id {
            body["message_thread_id"] = serde_json::Value::String(tid.to_string());
        }

        // Register the oneshot BEFORE sending the message to avoid a race
        // where the user taps the button before the sender is in the map.
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(approval_id.clone(), tx);

        let resp = self
            .http_client()
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .await;

        let send_ok = match resp {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                let status = r.status();
                let err = r.text().await.unwrap_or_default();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"status": status.to_string(), "err": err})
                        ),
                    "Telegram sendMessage (approval) with HTML failed; retrying without parse_mode"
                );

                // Fallback: plain text, no parse_mode, keep the buttons
                let plain_text = format!(
                    "🔧 Tool approval required\n\nTool: {}\n{}\n\nTap a button below:",
                    request.tool_name, request.arguments_summary
                );
                let mut plain_body = serde_json::json!({
                    "chat_id": chat_id,
                    "text": plain_text,
                    "reply_markup": reply_markup,
                });
                if let Some(tid) = thread_id {
                    plain_body["message_thread_id"] = serde_json::Value::String(tid.to_string());
                }

                let plain_resp = self
                    .http_client()
                    .post(self.api_url("sendMessage"))
                    .json(&plain_body)
                    .send()
                    .await;

                match plain_resp {
                    Ok(r) if r.status().is_success() => true,
                    Ok(r) => {
                        let status = r.status();
                        let err = r.text().await.unwrap_or_default();
                        self.pending_approvals.lock().await.remove(&approval_id);
                        anyhow::bail!("Telegram sendMessage (approval) failed ({status}): {err}");
                    }
                    Err(e) => {
                        self.pending_approvals.lock().await.remove(&approval_id);
                        return Err(e.into());
                    }
                }
            }
            Err(e) => {
                self.pending_approvals.lock().await.remove(&approval_id);
                return Err(e.into());
            }
        };

        if !send_ok {
            self.pending_approvals.lock().await.remove(&approval_id);
            anyhow::bail!("Telegram sendMessage (approval) failed after fallback");
        }

        // Wait for the user to tap a button. Timeout is configurable via
        // `channels.telegram.approval_timeout_secs` (default 120s).
        let result =
            match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), rx).await {
                Ok(Ok(response)) => Some(response),
                _ => {
                    // Timeout or sender dropped — clean up and deny.
                    self.pending_approvals.lock().await.remove(&approval_id);
                    Some(ChannelApprovalResponse::Deny)
                }
            };

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_masks_poll_error_url() {
        let raw = "error sending request for url (https://api.telegram.org/bot123456:ABC-def_GHI/getUpdates)";
        let redacted = zeroclaw_runtime::security::scrub(raw);
        assert!(!redacted.contains("123456:ABC-def_GHI"));
        assert!(redacted.contains("[REDACTED_BOT_TOKEN]"));
    }

    #[test]
    fn scrub_leaves_unrelated_text_untouched() {
        let raw = "connection reset by peer";
        assert_eq!(zeroclaw_runtime::security::scrub(raw), raw);
    }

    #[test]
    fn voice_peer_resolver_resolves_live_from_config() {
        use zeroclaw_config::multi_agent::{OutputModality, PeerGroupConfig, PeerUsername};

        let mut config = zeroclaw_config::schema::Config::default();
        // Voice group on this channel type — should be resolved.
        config.peer_groups.insert(
            "voicers".to_string(),
            PeerGroupConfig {
                channel: "telegram".into(),
                external_peers: vec![PeerUsername::new("@alice"), PeerUsername::new("@bob")],
                output_modality: OutputModality::Voice,
                ..Default::default()
            },
        );
        // Voice group on a different channel — must NOT leak into telegram.
        config.peer_groups.insert(
            "other".to_string(),
            PeerGroupConfig {
                channel: "signal".into(),
                external_peers: vec![PeerUsername::new("@carol")],
                output_modality: OutputModality::Voice,
                ..Default::default()
            },
        );
        // Mirror group on this channel — not a voice preference, skip.
        config.peer_groups.insert(
            "mirrorers".to_string(),
            PeerGroupConfig {
                channel: "telegram".into(),
                external_peers: vec![PeerUsername::new("@dave")],
                output_modality: OutputModality::Mirror,
                ..Default::default()
            },
        );

        let ch = TelegramChannel::new(
            "fake-token".into(),
            "default",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_voice_peer_resolver(Arc::new({
            let cfg = config.clone();
            move || cfg.channel_voice_peers("telegram", "default")
        }));

        // is_voice_chat resolves live via voice_peer_resolver — no cache.
        assert!(
            ch.is_voice_chat("@alice"),
            "voice peer should be recognized"
        );
        assert!(ch.is_voice_chat("@bob"), "voice peer should be recognized");
        assert!(
            !ch.is_voice_chat("@carol"),
            "peers on another channel must not be recognized"
        );
        assert!(
            !ch.is_voice_chat("@dave"),
            "mirror-modality peers must not be recognized"
        );

        // Live resolver must NOT pollute the session voice_chats set.
        let vc = ch.voice_chats.lock().unwrap();
        assert!(
            !vc.contains("@alice"),
            "live-resolved peers must not pollute the session voice_chats set"
        );
    }

    #[test]
    fn voice_peer_resolver_survives_session_voice_chats_removal() {
        use zeroclaw_config::multi_agent::{OutputModality, PeerGroupConfig, PeerUsername};

        let mut config = zeroclaw_config::schema::Config::default();
        config.peer_groups.insert(
            "voicers".to_string(),
            PeerGroupConfig {
                channel: "telegram".into(),
                external_peers: vec![PeerUsername::new("@alice")],
                output_modality: OutputModality::Voice,
                ..Default::default()
            },
        );

        let ch = TelegramChannel::new(
            "fake-token".into(),
            "default",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_voice_peer_resolver(Arc::new({
            let cfg = config.clone();
            move || cfg.channel_voice_peers("telegram", "default")
        }));

        // Simulate a voice-send removing @alice from voice_chats (even though
        // she was never in it — this proves live-resolved peers are separate).
        ch.voice_chats.lock().unwrap().remove("@alice");

        // is_voice_chat must still return true via voice_peer_resolver.
        assert!(
            ch.is_voice_chat("@alice"),
            "live-resolved voice peer must remain active after voice_chats removal"
        );
    }

    #[test]
    fn audio_send_spec_opus_is_voice_note() {
        // Only OGG/Opus becomes a real Telegram voice note.
        let (method, field, filename, mime) = telegram_audio_send_spec("opus").unwrap();
        assert_eq!(method, "sendVoice");
        assert_eq!(field, "voice");
        assert_eq!(filename, "voice.ogg");
        assert_eq!(mime, "audio/ogg");
        // "ogg" is an accepted alias for the same path.
        assert_eq!(telegram_audio_send_spec("ogg").unwrap().0, "sendVoice");
    }

    #[test]
    fn audio_send_spec_wav_uses_send_audio_with_real_mime() {
        // Groq Orpheus / Piper emit WAV — must not be mislabeled as audio/ogg.
        let (method, field, filename, mime) = telegram_audio_send_spec("wav").unwrap();
        assert_eq!(method, "sendAudio");
        assert_eq!(field, "audio");
        assert_eq!(filename, "voice.wav");
        assert_eq!(mime, "audio/wav");
    }

    #[test]
    fn audio_send_spec_mp3_uses_send_audio() {
        let (method, _field, filename, mime) = telegram_audio_send_spec("mp3").unwrap();
        assert_eq!(method, "sendAudio");
        assert_eq!(filename, "voice.mp3");
        assert_eq!(mime, "audio/mpeg");
    }

    #[test]
    fn audio_send_spec_is_case_and_whitespace_insensitive() {
        assert_eq!(telegram_audio_send_spec("  WAV ").unwrap().2, "voice.wav");
        assert_eq!(telegram_audio_send_spec("Opus").unwrap().0, "sendVoice");
    }

    #[test]
    fn audio_send_spec_pcm_is_rejected() {
        let err = telegram_audio_send_spec("pcm")
            .expect_err("pcm must be rejected — it is not a container format");
        assert!(err.to_string().contains("PCM"), "got: {err}");
    }

    #[test]
    fn audio_send_spec_unknown_format_falls_back_to_octet_stream() {
        let (method, _field, filename, mime) = telegram_audio_send_spec("speex").unwrap();
        assert_eq!(method, "sendAudio");
        assert_eq!(filename, "voice.bin");
        assert_eq!(mime, "application/octet-stream");
    }

    #[test]
    fn telegram_channel_name() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        assert_eq!(ch.name(), "telegram");
    }

    #[tokio::test]
    async fn telegram_with_transcription_binds_sole_provider_alias() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        // Only the Groq key is set -> exactly one provider registers.
        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test-groq-key".to_string()),
            ..zeroclaw_config::schema::TranscriptionConfig::default()
        };

        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_transcription(config);

        let manager = ch
            .transcription_manager
            .as_ref()
            .expect("single configured provider must build a transcription manager");

        // Alias is bound for the single-provider case. Stop before any network
        // call by using an unsupported audio format, which `validate_audio`
        // rejects first inside the provider's `transcribe`.
        let err = manager
            .transcribe(&[0u8; 16], "voice.aac")
            .await
            .expect_err("unsupported format must error before any network call");
        let msg = err.to_string();
        assert!(
            !msg.contains("no transcription_provider configured"),
            "alias must be bound for the single-provider case; got: {msg}"
        );
        assert!(
            msg.contains("Unsupported audio format"),
            "expected the bound provider to reach audio validation; got: {msg}"
        );
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

    fn media_group_update(
        update_id: i64,
        message_id: i64,
        chat_id: i64,
        media_group_id: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "update_id": update_id,
            "message": {
                "message_id": message_id,
                "media_group_id": media_group_id,
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": chat_id, "type": "private" },
                "photo": [{ "file_id": format!("file-{message_id}") }]
            }
        })
    }

    #[test]
    fn media_group_buffer_settles_at_exact_boundary() {
        let mut pending = std::collections::HashMap::new();
        let started = Instant::now();
        let update = media_group_update(1, 10, 100, "album");
        assert!(TelegramChannel::buffer_media_group_update(
            &mut pending,
            &update,
            started,
            1
        ));

        assert!(
            TelegramChannel::take_settled_media_groups(
                &mut pending,
                started + Duration::from_secs(5),
                1
            )
            .is_empty(),
            "a group cannot settle in the response that first observed it"
        );

        assert!(
            TelegramChannel::take_settled_media_groups(
                &mut pending,
                started + Duration::from_millis(699),
                2
            )
            .is_empty(),
            "699 ms is still inside the debounce window"
        );
        assert_eq!(
            TelegramChannel::take_settled_media_groups(
                &mut pending,
                started + Duration::from_millis(700),
                2
            )
            .len(),
            1,
            "700 ms settles the group"
        );
    }

    #[test]
    fn media_group_buffer_cross_poll_resets_deadline_dedupes_and_orders() {
        let mut pending = std::collections::HashMap::new();
        let started = Instant::now();
        let later = media_group_update(2, 12, 100, "album");
        let earlier = media_group_update(1, 11, 100, "album");

        assert!(TelegramChannel::buffer_media_group_update(
            &mut pending,
            &later,
            started,
            1
        ));
        assert!(TelegramChannel::buffer_media_group_update(
            &mut pending,
            &later,
            started + Duration::from_millis(100),
            2
        ));
        assert_eq!(pending.values().next().unwrap().updates.len(), 1);

        assert!(TelegramChannel::buffer_media_group_update(
            &mut pending,
            &earlier,
            started + Duration::from_millis(500),
            2
        ));
        assert!(
            TelegramChannel::take_settled_media_groups(
                &mut pending,
                started + Duration::from_millis(1199),
                3
            )
            .is_empty(),
            "a distinct member resets last_seen across poll responses"
        );

        assert!(
            TelegramChannel::take_settled_media_groups(
                &mut pending,
                started + Duration::from_millis(1200),
                2
            )
            .is_empty(),
            "the poll that observed a distinct member cannot settle it"
        );
        let batches = TelegramChannel::take_settled_media_groups(
            &mut pending,
            started + Duration::from_millis(1200),
            3,
        );
        let ids: Vec<i64> = batches[0]
            .iter()
            .filter_map(TelegramChannel::update_message_id)
            .collect();
        assert_eq!(ids, vec![11, 12]);
    }

    #[test]
    fn unsupported_media_group_member_refreshes_existing_group_without_being_stored() {
        let mut pending = std::collections::HashMap::new();
        let started = Instant::now();
        let photo = media_group_update(1, 10, 100, "album");
        let video = serde_json::json!({
            "update_id": 2,
            "message": {
                "message_id": 11,
                "media_group_id": "album",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": 100, "type": "private" },
                "video": { "file_id": "unsupported-video" }
            }
        });

        assert!(TelegramChannel::buffer_media_group_update(
            &mut pending,
            &photo,
            started,
            1
        ));
        assert!(TelegramChannel::buffer_media_group_update(
            &mut pending,
            &video,
            started + Duration::from_millis(600),
            2
        ));
        assert_eq!(pending.values().next().unwrap().updates.len(), 1);
        assert!(TelegramChannel::should_defer_media_group_update(
            &pending, &video
        ));
        assert!(
            TelegramChannel::take_settled_media_groups(
                &mut pending,
                started + Duration::from_millis(700),
                3
            )
            .is_empty(),
            "an unsupported sibling is still album activity"
        );
        assert_eq!(
            TelegramChannel::take_settled_media_groups(
                &mut pending,
                started + Duration::from_millis(1300),
                3
            )
            .len(),
            1
        );

        let mut unsupported_only = std::collections::HashMap::new();
        assert!(TelegramChannel::buffer_media_group_update(
            &mut unsupported_only,
            &video,
            started,
            1
        ));
        assert!(
            unsupported_only.is_empty(),
            "unsupported-only albums should not allocate pending state"
        );

        let grouped_audio = serde_json::json!({
            "update_id": 3,
            "message": {
                "message_id": 12,
                "media_group_id": "audio-album",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": 100, "type": "private" },
                "audio": { "file_id": "audio-file", "duration": 5 }
            }
        });
        assert!(
            !TelegramChannel::should_defer_media_group_update(&unsupported_only, &grouped_audio),
            "an unsupported-only group must keep its existing parser behavior"
        );
    }

    #[test]
    fn media_group_buffer_scopes_same_group_id_by_chat_and_orders_due_groups() {
        let mut pending = std::collections::HashMap::new();
        let started = Instant::now();
        let later = media_group_update(20, 20, 200, "same-id");
        let earlier = media_group_update(10, 10, 100, "same-id");
        TelegramChannel::buffer_media_group_update(&mut pending, &later, started, 1);
        TelegramChannel::buffer_media_group_update(&mut pending, &earlier, started, 1);

        assert_eq!(pending.len(), 2, "chat ID is part of the group key");
        assert_eq!(
            TelegramChannel::media_group_poll_timeout_secs(&pending),
            TELEGRAM_PENDING_MEDIA_GROUP_POLL_TIMEOUT_SECS
        );
        let batches = TelegramChannel::take_settled_media_groups(
            &mut pending,
            started + TELEGRAM_MEDIA_GROUP_SETTLE_DELAY,
            2,
        );
        assert_eq!(TelegramChannel::update_id(&batches[0][0]), Some(10));
        assert_eq!(TelegramChannel::update_id(&batches[1][0]), Some(20));
        assert_eq!(
            TelegramChannel::media_group_poll_timeout_secs(&pending),
            TELEGRAM_IDLE_POLL_TIMEOUT_SECS
        );
    }

    #[test]
    fn media_group_later_same_chat_update_takes_only_prior_groups() {
        let mut pending = std::collections::HashMap::new();
        let now = Instant::now();
        for update in [
            media_group_update(10, 10, 100, "prior"),
            media_group_update(11, 11, 100, "prior"),
            media_group_update(30, 30, 100, "later"),
            media_group_update(31, 31, 100, "later"),
            media_group_update(5, 5, 200, "other-chat"),
        ] {
            TelegramChannel::buffer_media_group_update(&mut pending, &update, now, 1);
        }
        let ordinary = serde_json::json!({
            "update_id": 20,
            "message": {
                "message_id": 20,
                "text": "follow up",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": 100, "type": "private" }
            }
        });

        let batches = TelegramChannel::take_prior_media_groups_for_update(&mut pending, &ordinary);
        assert_eq!(batches.len(), 1);
        let ids: Vec<i64> = batches[0]
            .iter()
            .filter_map(TelegramChannel::update_message_id)
            .collect();
        assert_eq!(ids, vec![10, 11]);
        assert!(pending.contains_key(&(100, "later".to_string())));
        assert!(pending.contains_key(&(200, "other-chat".to_string())));
    }

    #[test]
    fn prior_media_group_boundary_fails_closed_without_ordering_ids() {
        let mut pending = std::collections::HashMap::new();
        let now = Instant::now();
        let mut member = media_group_update(10, 10, 100, "album");
        member.as_object_mut().unwrap().remove("update_id");
        TelegramChannel::buffer_media_group_update(&mut pending, &member, now, 1);

        let ordinary = serde_json::json!({
            "update_id": 20,
            "message": {
                "message_id": 20,
                "text": "follow up",
                "chat": { "id": 100, "type": "private" }
            }
        });
        assert!(
            TelegramChannel::take_prior_media_groups_for_update(&mut pending, &ordinary).is_empty()
        );
        assert_eq!(pending.len(), 1);

        let mut missing_update_id = ordinary;
        missing_update_id
            .as_object_mut()
            .unwrap()
            .remove("update_id");
        assert!(
            TelegramChannel::take_prior_media_groups_for_update(&mut pending, &missing_update_id)
                .is_empty()
        );
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn typing_handle_starts_as_none() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let guard = ch.typing_handle.lock();
        assert!(guard.is_none());
    }

    #[tokio::test]
    async fn stop_typing_clears_handle() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );

        // Manually insert a dummy handle
        {
            let mut guard = ch.typing_handle.lock();
            *guard = Some(zeroclaw_spawn::spawn!(async {
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );

        // Insert a dummy handle first
        {
            let mut guard = ch.typing_handle.lock();
            *guard = Some(zeroclaw_spawn::spawn!(async {
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
        let mention_only = false;
        let off = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        assert!(!off.supports_draft_updates());

        let partial = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_streaming(StreamMode::Partial, 750);
        assert!(partial.supports_draft_updates());
        assert_eq!(partial.draft_update_interval_ms, 750);
    }

    #[test]
    fn with_streaming_uses_default_for_zero_draft_update_interval() {
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_streaming(StreamMode::Partial, 0);

        assert_eq!(
            ch.draft_update_interval_ms,
            TELEGRAM_DRAFT_UPDATE_INTERVAL_MS
        );
    }

    #[tokio::test]
    async fn send_draft_returns_none_when_stream_mode_off() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let id = ch
            .send_draft(&SendMessage::new("draft", "123"))
            .await
            .unwrap();
        assert!(id.is_none());
    }

    #[tokio::test]
    async fn update_draft_rate_limit_short_circuits_network() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_streaming(StreamMode::Partial, 60_000);
        ch.last_draft_edit
            .lock()
            .insert("123".to_string(), std::time::Instant::now());

        let result = ch.update_draft("123", "42", "delta text").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn update_draft_utf8_truncation_is_safe_for_multibyte_text() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_streaming(StreamMode::Partial, 0);
        let long_text = "a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH + 64);

        // For oversized text + invalid draft message_id, finalize_draft should
        // fall back to chunked send instead of returning early.
        let result = ch
            .finalize_draft("123", "not-a-number", &long_text, false)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn telegram_api_url() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert_eq!(
            ch.api_url("getMe"),
            "https://api.telegram.org/bot123:ABC/getMe"
        );
    }

    #[test]
    fn telegram_api_url_uses_custom_api_base() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        )
        .with_api_base("http://127.0.0.1:8081".to_string());

        assert_eq!(
            ch.api_url("getMe"),
            "http://127.0.0.1:8081/bot123:ABC/getMe"
        );
    }

    #[test]
    fn telegram_api_url_normalizes_custom_api_base_trailing_slash() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        )
        .with_api_base("http://127.0.0.1:8081/".to_string());

        assert_eq!(
            ch.api_url("getMe"),
            "http://127.0.0.1:8081/bot123:ABC/getMe"
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn telegram_user_allowed_specific() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".into(), "bob".into()]),
            mention_only,
        );
        assert!(ch.is_user_allowed("alice"));
        assert!(!ch.is_user_allowed("eve"));
    }

    #[test]
    fn telegram_user_allowed_with_at_prefix_in_config() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["@alice".into()]),
            mention_only,
        );
        assert!(ch.is_user_allowed("alice"));
    }

    #[test]
    fn telegram_user_denied_empty() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn telegram_user_exact_match_not_substring() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".into()]),
            mention_only,
        );
        assert!(!ch.is_user_allowed("alice_bot"));
        assert!(!ch.is_user_allowed("alic"));
        assert!(!ch.is_user_allowed("malice"));
    }

    #[test]
    fn telegram_user_empty_string_denied() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".into()]),
            mention_only,
        );
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn telegram_user_case_sensitive() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["Alice".into()]),
            mention_only,
        );
        assert!(ch.is_user_allowed("Alice"));
        assert!(!ch.is_user_allowed("alice"));
        assert!(!ch.is_user_allowed("ALICE"));
    }

    #[test]
    fn telegram_wildcard_with_specific_users() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".into(), "*".into()]),
            mention_only,
        );
        assert!(ch.is_user_allowed("alice"));
        assert!(ch.is_user_allowed("bob"));
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn telegram_user_allowed_by_numeric_id_identity() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["123456789".into()]),
            mention_only,
        );
        assert!(ch.is_any_user_allowed(["unknown", "123456789"]));
    }

    #[test]
    fn telegram_user_denied_when_none_of_identities_match() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".into(), "987654321".into()]),
            mention_only,
        );
        assert!(!ch.is_any_user_allowed(["unknown", "123456789"]));
    }

    #[test]
    fn telegram_pairing_enabled_with_empty_allowlist() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert!(ch.pairing_code_active());
    }

    #[test]
    fn telegram_pairing_disabled_with_nonempty_allowlist() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".into()]),
            mention_only,
        );
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
    fn suggested_bind_command_omits_alias_flag_for_default() {
        // The CLI defaults to the `default` alias, so the short form must
        // stay byte-identical for existing default-alias users.
        assert_eq!(
            TelegramChannel::suggested_bind_command("default", "123456789"),
            "zeroclaw channel bind-telegram 123456789"
        );
    }

    #[test]
    fn suggested_bind_command_appends_alias_flag_for_non_default() {
        // A non-default agent must get the `--alias` flag or the operator's
        // copy-pasted command binds the wrong peer group and the bot keeps
        // asking for approval.
        assert_eq!(
            TelegramChannel::suggested_bind_command("alerts", "123456789"),
            "zeroclaw channel bind-telegram 123456789 --alias alerts"
        );
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
    fn parse_attachment_markers_keeps_invalid_markers_in_text() {
        let message = "Report [UNKNOWN:/tmp/a.bin]";
        let (cleaned, attachments) = parse_attachment_markers(message);

        assert_eq!(cleaned, "Report [UNKNOWN:/tmp/a.bin]");
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
    fn infer_attachment_kind_from_target_detects_document_extension() {
        assert_eq!(
            infer_attachment_kind_from_target("https://example.com/files/specs.pdf?download=1"),
            Some(TelegramAttachmentKind::Document)
        );
    }

    #[test]
    fn parse_update_message_uses_chat_id_as_reply_target() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["555".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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

    // ── File sending API URL tests ──────────────────────────────────

    #[test]
    fn telegram_api_url_send_document() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert_eq!(
            ch.api_url("sendDocument"),
            "https://api.telegram.org/bot123:ABC/sendDocument"
        );
    }

    #[test]
    fn telegram_api_url_send_photo() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert_eq!(
            ch.api_url("sendPhoto"),
            "https://api.telegram.org/bot123:ABC/sendPhoto"
        );
    }

    #[test]
    fn telegram_api_url_send_video() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert_eq!(
            ch.api_url("sendVideo"),
            "https://api.telegram.org/bot123:ABC/sendVideo"
        );
    }

    #[test]
    fn telegram_api_url_send_audio() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert_eq!(
            ch.api_url("sendAudio"),
            "https://api.telegram.org/bot123:ABC/sendAudio"
        );
    }

    #[test]
    fn telegram_api_url_send_voice() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "123:ABC".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            mention_only,
        );
        assert_eq!(
            ch.api_url("sendVoice"),
            "https://api.telegram.org/bot123:ABC/sendVoice"
        );
    }

    // ── File sending integration tests (with mock server) ──────────

    #[tokio::test]
    async fn telegram_send_document_bytes_builds_correct_form() {
        // This test verifies the method doesn't panic and handles bytes correctly
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        // Minimal valid PNG header bytes
        let file_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

        let result = ch
            .send_photo_bytes("123456", None, file_bytes, "test.png", None)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_document_by_url_builds_correct_json() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );

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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );

        let result = ch
            .send_photo_by_url("123456", None, "https://example.com/image.jpg", None)
            .await;

        assert!(result.is_err());
    }

    // ── File path handling tests ────────────────────────────────────

    #[tokio::test]
    async fn telegram_send_document_nonexistent_file() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let path = Path::new("/nonexistent/path/to/photo.jpg");

        let result = ch.send_photo("123456", None, path, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_video_nonexistent_file() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let path = Path::new("/nonexistent/path/to/video.mp4");

        let result = ch.send_video("123456", None, path, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_audio_nonexistent_file() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let path = Path::new("/nonexistent/path/to/audio.mp3");

        let result = ch.send_audio("123456", None, path, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_voice_nonexistent_file() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
    fn telegram_split_counts_final_continued_marker_in_send_length() {
        let msg = "a".repeat(8142);
        let chunks = split_message_for_telegram(&msg);
        assert!(chunks.len() >= 2);

        for (index, chunk) in chunks.iter().enumerate() {
            let text = format_telegram_text_chunk(chunk, index, chunks.len());
            assert!(
                text.chars().count() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "final sent chunk {index} must be <= {TELEGRAM_MAX_MESSAGE_LENGTH}, got {}",
                text.chars().count()
            );
        }

        let final_text =
            format_telegram_text_chunk(chunks.last().unwrap(), chunks.len() - 1, chunks.len());
        assert!(final_text.starts_with(TELEGRAM_CONTINUED_PREFIX));
    }

    #[test]
    fn telegram_split_counts_middle_continuation_markers_in_send_length() {
        let msg = "a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH * 3);
        let chunks = split_message_for_telegram(&msg);
        assert!(chunks.len() >= 3);

        for (index, chunk) in chunks.iter().enumerate() {
            let text = format_telegram_text_chunk(chunk, index, chunks.len());
            assert!(
                text.chars().count() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "sent chunk {index} must be <= {TELEGRAM_MAX_MESSAGE_LENGTH}, got {}",
                text.chars().count()
            );
        }

        let middle = format_telegram_text_chunk(&chunks[1], 1, chunks.len());
        assert!(middle.starts_with(TELEGRAM_CONTINUED_PREFIX));
        assert!(middle.ends_with(TELEGRAM_CONTINUES_SUFFIX));
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/sendDocument$"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({ "ok": false, "description": "empty document rejected" }),
            ))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_api_base(mock_server.uri());
        let file_bytes: Vec<u8> = vec![];

        let result = ch
            .send_document_bytes("123456", None, file_bytes, "empty.txt", None)
            .await;

        let err = result.expect_err("empty document send should fail");
        assert!(
            err.to_string().contains("empty document rejected"),
            "expected mocked Telegram error, got: {err}"
        );
    }

    #[tokio::test]
    async fn telegram_send_document_bytes_empty_filename() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let file_bytes = b"content".to_vec();

        let result = ch
            .send_document_bytes("123456", None, file_bytes, "", None)
            .await;

        // Should not panic
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn telegram_send_document_bytes_empty_chat_id() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
    fn telegram_normalize_incoming_content_preserves_mention() {
        let result = TelegramChannel::normalize_incoming_content("@mybot hello", "mybot");
        assert_eq!(result, Some("@mybot hello".to_string()));
    }

    #[test]
    fn telegram_normalize_incoming_content_returns_none_for_empty() {
        let result = TelegramChannel::normalize_incoming_content("   ", "mybot");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_update_message_mention_only_group_requires_exact_mention() {
        let mention_only = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
    fn parse_update_message_mention_only_group_preserves_mention_in_body() {
        let mention_only = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        assert_eq!(parsed.content, "Hi @MyBot status please");

        let mention_only_update = serde_json::json!({
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

        let parsed = ch
            .parse_update_message(&mention_only_update)
            .expect("mention-only body admits");
        assert_eq!(parsed.content, "@mybot");
    }

    #[test]
    fn parse_update_reply_to_bot_bypasses_mention_only_gate() {
        let mention_only = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }
        {
            let mut cache = ch.bot_id.lock();
            *cache = Some(42);
        }

        // Reply to the bot's own message — no mention needed.
        let update = serde_json::json!({
            "update_id": 20,
            "message": {
                "message_id": 55,
                "text": "do this",
                "from": { "id": 555, "username": "alice" },
                "chat": { "id": -100_200_300, "type": "group" },
                "reply_to_message": {
                    "message_id": 50,
                    "from": { "id": 42, "username": "mybot", "is_bot": true },
                    "text": "original"
                }
            }
        });

        let parsed = ch
            .parse_update_message(&update)
            .expect("reply-to-bot should bypass mention_only gate");
        // extract_reply_context prepends the quote; the gate returns the body,
        // and the quote is re-added by the normal reply-handling path.
        assert_eq!(parsed.content, "> @mybot:\n> original\n\ndo this");
    }

    #[test]
    fn parse_update_reply_to_non_bot_still_dropped_in_mention_only() {
        let mention_only = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }
        {
            let mut cache = ch.bot_id.lock();
            *cache = Some(42);
        }

        // Reply to another user (not the bot) — still needs a mention.
        let update = serde_json::json!({
            "update_id": 21,
            "message": {
                "message_id": 56,
                "text": "hello",
                "from": { "id": 555, "username": "alice" },
                "chat": { "id": -100_200_300, "type": "group" },
                "reply_to_message": {
                    "message_id": 51,
                    "from": { "id": 99, "username": "charlie" },
                    "text": "some message"
                }
            }
        });

        assert!(ch.parse_update_message(&update).is_none());
    }

    #[test]
    fn parse_update_reply_bot_id_unresolved_falls_through_in_mention_only() {
        let mention_only = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }
        // bot_id stays None — unresolved.

        // Reply to the bot's message, but bot_id is unresolved — falls through.
        let update = serde_json::json!({
            "update_id": 22,
            "message": {
                "message_id": 57,
                "text": "hello",
                "from": { "id": 555, "username": "alice" },
                "chat": { "id": -100_200_300, "type": "group" },
                "reply_to_message": {
                    "message_id": 52,
                    "from": { "id": 42, "username": "mybot", "is_bot": true },
                    "text": "original"
                }
            }
        });

        assert!(ch.parse_update_message(&update).is_none());
    }

    #[test]
    fn parse_update_reply_to_bot_bypasses_mention_only_gate_caption_path() {
        let mention_only = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }
        {
            let mut cache = ch.bot_id.lock();
            *cache = Some(42);
        }

        // Photo with a caption, replying to the bot — caption should pass.
        // This exercises check_media_mention_gate directly because
        // parse_update_message requires `message.text` and photo updates
        // carry only `message.caption`.
        let message = serde_json::json!({
            "message_id": 58,
            "caption": "enhance this",
            "from": { "id": 555, "username": "alice" },
            "chat": { "id": -100_200_300, "type": "group" },
            "photo": [
                { "file_id": "abc", "width": 100, "height": 100 }
            ],
            "reply_to_message": {
                "message_id": 53,
                "from": { "id": 42, "username": "mybot", "is_bot": true },
                "text": "original photo"
            }
        });

        let result = ch.check_media_mention_gate(&message, Some("enhance this"));
        assert!(
            result.is_some(),
            "reply-to-bot caption should bypass mention_only gate"
        );
        let gated = result.unwrap();
        assert!(gated.is_some(), "gate should return the normalized caption");
        assert_eq!(gated.unwrap(), "enhance this");
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
        let mention_only = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        assert!(ch.mention_only);

        let disabled_mention_only = false;
        let ch_disabled = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            disabled_mention_only,
        );
        assert!(!ch_disabled.mention_only);
    }

    fn group_message_with_caption(caption: Option<&str>) -> serde_json::Value {
        let mut msg = serde_json::json!({
            "message_id": 1,
            "from": { "id": 1, "username": "alice" },
            "chat": { "id": -1, "type": "group" }
        });
        if let Some(c) = caption {
            msg["caption"] = serde_json::Value::String(c.to_string());
        }
        msg
    }

    #[test]
    fn check_media_mention_gate_rejects_group_media_without_mention() {
        let ch = TelegramChannel::new(
            "token".into(),
            "default",
            std::sync::Arc::new(|| vec!["*".into()]),
            true,
        );
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }
        let no_caption = group_message_with_caption(None);
        assert!(
            ch.check_media_mention_gate(&no_caption, None).is_none(),
            "no caption + mention_only group ⇒ reject"
        );
        let unrelated_caption = group_message_with_caption(Some("nice photo"));
        assert!(
            ch.check_media_mention_gate(&unrelated_caption, Some("nice photo"))
                .is_none(),
            "caption without bot mention + mention_only group ⇒ reject"
        );
        let other_bot_caption = group_message_with_caption(Some("hey @otherbot look"));
        assert!(
            ch.check_media_mention_gate(&other_bot_caption, Some("hey @otherbot look"))
                .is_none(),
            "caption mentioning a different bot ⇒ reject"
        );
    }

    #[test]
    fn check_media_mention_gate_admits_and_preserves_caption_mention() {
        let ch = TelegramChannel::new(
            "token".into(),
            "default",
            std::sync::Arc::new(|| vec!["*".into()]),
            true,
        );
        {
            let mut cache = ch.bot_username.lock();
            *cache = Some("mybot".to_string());
        }
        let msg = group_message_with_caption(Some("@mybot describe this"));
        let result = ch.check_media_mention_gate(&msg, Some("@mybot describe this"));
        assert_eq!(
            result,
            Some(Some("@mybot describe this".to_string())),
            "mention text preserved verbatim once gate admits"
        );
    }

    #[test]
    fn check_media_mention_gate_passes_dm_unchanged() {
        let ch = TelegramChannel::new(
            "token".into(),
            "default",
            std::sync::Arc::new(|| vec!["*".into()]),
            true,
        );
        let dm = serde_json::json!({
            "message_id": 1,
            "from": { "id": 1, "username": "alice" },
            "chat": { "id": 1, "type": "private" },
            "caption": "hello"
        });
        assert_eq!(
            ch.check_media_mention_gate(&dm, Some("hello")),
            Some(Some("hello".to_string())),
            "DM media must always pass with caption verbatim"
        );
        let dm_no_caption = serde_json::json!({
            "message_id": 1,
            "from": { "id": 1, "username": "alice" },
            "chat": { "id": 1, "type": "private" }
        });
        assert_eq!(
            ch.check_media_mention_gate(&dm_no_caption, None),
            Some(None),
            "DM media with no caption must pass"
        );
    }

    #[test]
    fn check_media_mention_gate_passes_when_mention_only_disabled() {
        let ch = TelegramChannel::new(
            "token".into(),
            "default",
            std::sync::Arc::new(|| vec!["*".into()]),
            false,
        );
        let group_no_caption = group_message_with_caption(None);
        assert_eq!(
            ch.check_media_mention_gate(&group_no_caption, None),
            Some(None),
            "mention_only off ⇒ all media pass"
        );
    }

    #[test]
    fn check_media_mention_gate_rejects_group_when_bot_username_unknown() {
        let ch = TelegramChannel::new(
            "token".into(),
            "default",
            std::sync::Arc::new(|| vec!["*".into()]),
            true,
        );
        // Do NOT set bot_username — leave it None.
        let group = group_message_with_caption(Some("@somebody hi"));
        assert!(
            ch.check_media_mention_gate(&group, Some("@somebody hi"))
                .is_none(),
            "missing bot_username in group must fail closed"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG6: Channel platform limit edge cases for Telegram (4096 char limit)
    // Prevents: Pattern 6 — issues
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
    fn telegram_split_long_fenced_code_block_balances_each_chunk() {
        let mut msg = String::new();
        msg.push_str("Intro\n\n```rust\n");
        for i in 0..700 {
            let _ = writeln!(msg, "fn generated_{i}() {{ println!(\"line {i:03}\"); }}");
        }
        msg.push_str("```\n\nOutro");

        let parts = split_message_for_telegram(&msg);
        assert!(parts.len() >= 2, "long fenced code block should split");
        for part in &parts {
            assert!(
                part.len() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "balanced chunk must be <= {TELEGRAM_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
            assert_eq!(
                part.matches("```").count() % 2,
                0,
                "each chunk should have balanced markdown fences"
            );

            let html = TelegramChannel::markdown_to_telegram_html(part);
            assert_eq!(
                html.matches("<pre><code>").count(),
                html.matches("</code></pre>").count(),
                "rendered Telegram HTML should have balanced code blocks"
            );
        }

        assert!(
            parts.iter().skip(1).any(|part| part.starts_with("```\n")),
            "continuation inside a code block should reopen a fence"
        );
        assert!(
            parts
                .iter()
                .take(parts.len() - 1)
                .any(|part| part.ends_with("\n```") || part.ends_with("```")),
            "split chunks inside a code block should close the fence"
        );
    }

    #[test]
    fn telegram_split_fenced_code_send_text_stays_within_limit_and_balanced() {
        let mut msg = String::new();
        msg.push_str("```rust\n");
        msg.push_str(&"a".repeat(TELEGRAM_MAX_MESSAGE_LENGTH + 120));
        msg.push_str("\n```\n");

        let parts = split_message_for_telegram(&msg);
        assert!(parts.len() >= 2);

        for (index, part) in parts.iter().enumerate() {
            let text = format_telegram_text_chunk(part, index, parts.len());
            assert!(
                text.chars().count() <= TELEGRAM_MAX_MESSAGE_LENGTH,
                "sent fenced chunk {index} must be <= {TELEGRAM_MAX_MESSAGE_LENGTH}, got {}",
                text.chars().count()
            );
            assert_eq!(
                text.matches("```").count() % 2,
                0,
                "sent fenced chunk {index} should have balanced markdown fences"
            );

            let html = TelegramChannel::markdown_to_telegram_html(&text);
            assert_eq!(
                html.matches("<pre><code>").count(),
                html.matches("</code></pre>").count(),
                "sent fenced chunk {index} should render balanced Telegram HTML"
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
        let msg: String = (0..1000).fold(String::new(), |mut acc, i| {
            let _ = writeln!(acc, "line {i}");
            acc
        });
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
        let (file_id, dur) = TelegramChannel::parse_voice_metadata(&msg).unwrap();
        assert_eq!(file_id, "abc123");
        assert_eq!(dur, 5);
    }

    #[test]
    fn parse_voice_metadata_extracts_audio() {
        let msg = serde_json::json!({
            "audio": {
                "file_id": "audio456",
                "duration": 30
            }
        });
        let (file_id, dur) = TelegramChannel::parse_voice_metadata(&msg).unwrap();
        assert_eq!(file_id, "audio456");
        assert_eq!(dur, 30);
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
        let (_, dur) = TelegramChannel::parse_voice_metadata(&msg).unwrap();
        assert_eq!(dur, 0);
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let msg = serde_json::json!({
            "text": "just a regular message"
        });
        assert!(ch.extract_reply_context(&msg).is_none());
    }

    #[test]
    fn extract_reply_context_skips_topic_root() {
        // Telegram auto-injects a reply_to_message pointing at the topic-root
        // message on every message in a non-General forum topic. The injected
        // reply's message_id equals the parent's message_thread_id. It is
        // not a real reply and must not produce a blockquote prefix.
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let msg = serde_json::json!({
            "message_thread_id": 42,
            "text": "hello in topic",
            "reply_to_message": {
                "message_id": 42,
                "from": { "username": "alice" },
                "forum_topic_created": { "name": "General Discussion", "icon_color": 0 }
            }
        });
        assert!(ch.extract_reply_context(&msg).is_none());
    }

    #[test]
    fn extract_reply_context_real_reply_in_topic() {
        // A genuine reply inside a forum topic (reply.message_id differs from
        // the parent's message_thread_id) should still produce a blockquote.
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let msg = serde_json::json!({
            "message_thread_id": 42,
            "text": "I agree",
            "reply_to_message": {
                "message_id": 100,
                "from": { "username": "alice" },
                "text": "What do you think?"
            }
        });
        let ctx = ch.extract_reply_context(&msg).unwrap();
        assert_eq!(ctx, "> @alice:\n> What do you think?");
    }

    #[test]
    fn extract_reply_context_no_username_uses_first_name() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            ..zeroclaw_config::schema::TranscriptionConfig::default()
        };

        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_transcription(tc);
        assert!(ch.transcription.is_some());
        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    fn with_transcription_skips_when_disabled() {
        let tc = zeroclaw_config::schema::TranscriptionConfig::default(); // enabled = false
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_transcription(tc);
        assert!(ch.transcription.is_none());
        assert!(ch.transcription_manager.is_none());
    }

    #[tokio::test]
    async fn try_parse_voice_message_returns_none_when_transcription_disabled() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            max_duration_secs: 5,
            ..Default::default()
        };

        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
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
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            max_duration_secs: 120,
            ..Default::default()
        };

        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".into()]),
            mention_only,
        )
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

    #[tokio::test]
    #[ignore = "requires GROQ_API_KEY environment variable"]
    async fn e2e_live_voice_transcription_and_reply_cache() {
        let Ok(api_key) = std::env::var("GROQ_API_KEY") else {
            eprintln!("GROQ_API_KEY not set — skipping live voice transcription test");
            return;
        };

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

        // 2. Call TranscriptionManager.transcribe() — real Groq Whisper API
        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some(api_key),
            ..Default::default()
        };
        let manager = crate::transcription::TranscriptionManager::new(&config)
            .expect("TranscriptionManager::new should succeed with valid GROQ_API_KEY");
        let transcript: String = manager
            .transcribe(&audio_data, "hello.mp3")
            .await
            .expect("transcribe should succeed with valid GROQ_API_KEY");

        // 3. Verify Whisper actually recognized "hello"
        assert!(
            transcript.to_lowercase().contains("hello"),
            "expected transcription to contain 'hello', got: '{transcript}'"
        );

        // 4. Create TelegramChannel, insert transcription into voice_transcriptions cache
        let mention_only = false;
        let ch = TelegramChannel::new(
            "test_token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
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
                {"file_id": "photo_id", "file_size": 1000}
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
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
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

    #[tokio::test]
    async fn media_group_listener_ignores_unsupported_member_without_splitting_album() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        let server = MockServer::start().await;
        let first = media_group_update(10, 100, 100, "album");
        let unsupported_video = serde_json::json!({
            "update_id": 11,
            "message": {
                "message_id": 101,
                "media_group_id": "album",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": 100, "type": "private" },
                "video": { "file_id": "unsupported-video" }
            }
        });
        let second = media_group_update(12, 102, 100, "album");
        let follow_up = serde_json::json!({
            "update_id": 13,
            "message": {
                "message_id": 103,
                "text": "after album",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": 100, "type": "private" }
            }
        });
        let poll_index = Arc::new(AtomicUsize::new(0));
        let responder_index = Arc::clone(&poll_index);
        Mock::given(method("POST"))
            .and(path("/botfake-token/getUpdates"))
            .respond_with(move |request: &Request| {
                let body: serde_json::Value = request.body_json().unwrap();
                if body.get("timeout").and_then(serde_json::Value::as_u64) == Some(0) {
                    return ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({ "ok": true, "result": [] }));
                }

                let result = match responder_index.fetch_add(1, Ordering::SeqCst) {
                    0 => vec![first.clone()],
                    1 => vec![unsupported_video.clone()],
                    2 => vec![second.clone()],
                    3 => vec![follow_up.clone()],
                    _ => Vec::new(),
                };
                let response = ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true, "result": result }));
                if responder_index.load(Ordering::SeqCst) >= 4 {
                    response.set_delay(Duration::from_millis(750))
                } else {
                    response
                }
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botfake-token/setMyCommands"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botfake-token/sendChatAction"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botfake-token/setMessageReaction"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .mount(&server)
            .await;
        for (file_id, file_path) in [
            ("file-100", "photos/first.jpg"),
            ("file-102", "photos/second.jpg"),
        ] {
            Mock::given(method("GET"))
                .and(path("/botfake-token/getFile"))
                .and(query_param("file_id", file_id))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "result": { "file_path": file_path }
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/file/botfake-token/{file_path}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b"image"))
                .expect(1)
                .mount(&server)
                .await;
        }

        let workspace = tempfile::tempdir().unwrap();
        let channel = Arc::new(
            TelegramChannel::new(
                "fake-token".into(),
                "default",
                Arc::new(|| vec!["alice".into()]),
                false,
            )
            .with_api_base(server.uri())
            .with_workspace_dir(workspace.path().to_path_buf())
            .with_ack_reactions(true),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listener_channel = Arc::clone(&channel);
        let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

        let album = tokio::time::timeout(Duration::from_secs(4), rx.recv())
            .await
            .expect("album should dispatch")
            .expect("listener should remain connected");
        let follow_up = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("follow-up should dispatch")
            .expect("listener should remain connected");
        assert_eq!(album.id, "telegram_100_100");
        assert_eq!(album.content.matches("[IMAGE:").count(), 2);
        assert_eq!(follow_up.content, "after album");
        assert!(rx.try_recv().is_err(), "album must dispatch exactly once");

        for _ in 0..50 {
            let requests = server.received_requests().await.unwrap();
            let reaction_count = requests
                .iter()
                .filter(|request| request.url.path() == "/botfake-token/setMessageReaction")
                .count();
            if reaction_count == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        listener.abort();
        let _ = listener.await;

        let requests = server.received_requests().await.unwrap();
        let typing_count = requests
            .iter()
            .filter(|request| request.url.path() == "/botfake-token/sendChatAction")
            .count();
        assert_eq!(typing_count, 2, "one typing action per logical message");
        let reaction_message_ids: std::collections::HashSet<i64> = requests
            .iter()
            .filter(|request| request.url.path() == "/botfake-token/setMessageReaction")
            .filter_map(|request| request.body_json::<serde_json::Value>().ok())
            .filter_map(|body| body.get("message_id").and_then(serde_json::Value::as_i64))
            .collect();
        assert_eq!(
            reaction_message_ids,
            std::collections::HashSet::from([100, 103])
        );
        let poll_timeouts: Vec<u64> = requests
            .iter()
            .filter(|request| request.url.path() == "/botfake-token/getUpdates")
            .filter_map(|request| request.body_json::<serde_json::Value>().ok())
            .filter_map(|body| body.get("timeout").and_then(serde_json::Value::as_u64))
            .filter(|timeout| *timeout > 0)
            .collect();
        assert!(poll_timeouts.starts_with(&[30, 1, 1, 1]));
    }

    #[tokio::test]
    async fn media_group_materializes_once_in_message_order_with_shared_context() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for (file_id, file_path, bytes) in [
            ("first-file", "photos/first.jpg", b"first".as_slice()),
            ("second-file", "photos/second.jpg", b"second".as_slice()),
        ] {
            Mock::given(method("GET"))
                .and(path("/botfake-token/getFile"))
                .and(query_param("file_id", file_id))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "result": { "file_path": file_path }
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/file/botfake-token/{file_path}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
                .expect(1)
                .mount(&server)
                .await;
        }

        let workspace = tempfile::tempdir().unwrap();
        let channel = TelegramChannel::new(
            "fake-token".into(),
            "default",
            Arc::new(|| vec!["alice".into()]),
            true,
        )
        .with_api_base(server.uri())
        .with_workspace_dir(workspace.path().to_path_buf());
        *channel.bot_username.lock() = Some("mybot".to_string());

        let first = serde_json::json!({
            "update_id": 10,
            "message": {
                "message_id": 10,
                "message_thread_id": 77,
                "media_group_id": "album-1",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": -100, "type": "group" },
                "photo": [{ "file_id": "first-file", "file_size": 5 }],
                "caption": "context"
            }
        });
        let second = serde_json::json!({
            "update_id": 11,
            "message": {
                "message_id": 11,
                "message_thread_id": 77,
                "media_group_id": "album-1",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": -100, "type": "group" },
                "photo": [{ "file_id": "second-file", "file_size": 6 }],
                "caption": "  @mybot compare these  "
            }
        });
        let oversized = serde_json::json!({
            "update_id": 9,
            "message": {
                "message_id": 9,
                "message_thread_id": 77,
                "media_group_id": "album-1",
                "from": { "id": 7, "username": "alice" },
                "chat": { "id": -100, "type": "group" },
                "photo": [{
                    "file_id": "oversized-file",
                    "file_size": TELEGRAM_MAX_FILE_DOWNLOAD_BYTES + 1
                }],
                "reply_to_message": {
                    "message_id": 8,
                    "from": { "id": 8, "username": "bob" },
                    "text": "prior"
                },
                "forward_origin": {
                    "type": "hidden_user",
                    "sender_user_name": "Original Sender"
                }
            }
        });

        let msg = channel
            .try_parse_media_group(&[oversized, second, first])
            .await
            .expect("valid siblings should survive one failed member");

        assert_eq!(
            msg.id, "telegram_-100_9",
            "the earliest real member remains the anchor even when its file is skipped"
        );
        assert_eq!(msg.reply_target, "-100:77");
        assert_eq!(msg.thread_ts.as_deref(), Some("77"));
        assert_eq!(msg.content.matches("[IMAGE:").count(), 2);
        let first_pos = msg.content.find("photo_-100_10.jpg").unwrap();
        let second_pos = msg.content.find("photo_-100_11.jpg").unwrap();
        assert!(
            first_pos < second_pos,
            "attachments follow message_id order"
        );
        assert_eq!(msg.content.matches("@mybot compare these").count(), 1);
        assert_eq!(msg.content.matches("context").count(), 1);
        assert_eq!(msg.content.matches("> @bob:").count(), 1);
        assert_eq!(
            msg.content
                .matches("[Forwarded from Original Sender]")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn media_group_same_named_documents_keep_distinct_files() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for (file_id, file_path, bytes) in [
            ("doc-one", "documents/one.bin", b"first".as_slice()),
            ("doc-two", "documents/two.bin", b"second".as_slice()),
        ] {
            Mock::given(method("GET"))
                .and(path("/botfake-token/getFile"))
                .and(query_param("file_id", file_id))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "result": { "file_path": file_path }
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/file/botfake-token/{file_path}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
                .expect(1)
                .mount(&server)
                .await;
        }

        let workspace = tempfile::tempdir().unwrap();
        let channel = TelegramChannel::new(
            "fake-token".into(),
            "default",
            Arc::new(|| vec!["alice".into()]),
            false,
        )
        .with_api_base(server.uri())
        .with_workspace_dir(workspace.path().to_path_buf());
        let document_update = |update_id: i64, file_id: &str| {
            serde_json::json!({
                "update_id": update_id,
                "message": {
                    "message_id": update_id,
                    "media_group_id": "documents",
                    "from": { "id": 7, "username": "alice" },
                    "chat": { "id": 100, "type": "private" },
                    "document": {
                        "file_id": file_id,
                        "file_name": "../report.pdf"
                    }
                }
            })
        };

        let msg = channel
            .try_parse_media_group(&[
                document_update(10, "doc-one"),
                document_update(11, "doc-two"),
            ])
            .await
            .expect("both documents should materialize");

        assert_eq!(msg.content.matches("[Document: report.pdf]").count(), 2);
        let save_dir = workspace.path().join("telegram_files");
        let first_path = save_dir.join("document_100_10_report.pdf");
        let second_path = save_dir.join("document_100_11_report.pdf");
        assert_eq!(tokio::fs::read(&first_path).await.unwrap(), b"first");
        assert_eq!(tokio::fs::read(&second_path).await.unwrap(), b"second");
        assert!(msg.content.contains(&first_path.display().to_string()));
        assert!(msg.content.contains(&second_path.display().to_string()));
    }

    #[tokio::test]
    async fn media_group_rejects_mixed_sender_before_download() {
        use wiremock::MockServer;

        let server = MockServer::start().await;
        let workspace = tempfile::tempdir().unwrap();
        let channel = TelegramChannel::new(
            "fake-token".into(),
            "default",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_api_base(server.uri())
        .with_workspace_dir(workspace.path().to_path_buf());
        let first = media_group_update(1, 1, 100, "album");
        let mut second = media_group_update(2, 2, 100, "album");
        second["message"]["from"]["id"] = serde_json::json!(8);

        assert!(
            channel
                .try_parse_media_group(&[first, second])
                .await
                .is_none(),
            "mixed sender album must fail closed"
        );
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "scope validation must happen before any file request"
        );
    }

    // ── Attachment content format tests ──────────────────────────────

    #[test]
    fn media_group_document_storage_names_are_safe_and_unique() {
        let display = safe_attachment_filename("../reports/report.pdf");
        assert_eq!(display, "report.pdf");
        assert_ne!(
            media_group_document_storage_filename(&display, "-100", 10),
            media_group_document_storage_filename(&display, "-100", 11)
        );
    }

    /// Photo attachments with image extension must use `[IMAGE:/path]` marker
    /// so the multimodal pipeline validates vision capability on the model_provider.
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

    #[test]
    fn attachment_document_content_uses_document_label() {
        let local_path = std::path::Path::new("/tmp/workspace/report.pdf");
        let local_filename = "report.pdf";

        let content =
            format_attachment_content(IncomingAttachmentKind::Document, local_filename, local_path);

        assert_eq!(content, "[Document: report.pdf] /tmp/workspace/report.pdf");
        assert!(!content.contains("[IMAGE:"));
    }

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

    #[test]
    fn markdown_attachment_not_detected_by_multimodal_image_markers() {
        let content = format_attachment_content(
            IncomingAttachmentKind::Photo,
            "notes.md",
            std::path::Path::new("/tmp/ws/notes.md"),
        );
        let messages = vec![zeroclaw_providers::ChatMessage::user(content)];
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&messages),
            0,
            "markdown file must not trigger image marker detection"
        );
    }

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

    #[test]
    fn photo_image_marker_detected_by_multimodal() {
        let photo_content = "[IMAGE:/tmp/workspace/photo_1_2.jpg]";
        let messages = vec![zeroclaw_providers::ChatMessage::user(
            photo_content.to_string(),
        )];
        let count = zeroclaw_providers::multimodal::count_image_markers(&messages);
        assert_eq!(
            count, 1,
            "multimodal should detect exactly one image marker"
        );
    }

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
        let messages = vec![zeroclaw_providers::ChatMessage::user(content)];
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&messages),
            1
        );
    }

    // ── E2E: attachment saves file and formats content ───────────────

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
        let doc_msgs = vec![zeroclaw_providers::ChatMessage::user(doc_content)];
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&doc_msgs),
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
        let photo_msgs = vec![zeroclaw_providers::ChatMessage::user(photo_content.clone())];
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&photo_msgs),
            1,
            "multimodal must detect exactly one image marker in photo content"
        );

        // ── Photo with caption ───────────────────────────────────────
        let mut captioned = photo_content;
        use std::fmt::Write;
        let _ = write!(captioned, "\n\nCheck this out");
        let cap_msgs = vec![zeroclaw_providers::ChatMessage::user(captioned.clone())];
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&cap_msgs),
            1,
            "caption must not break image marker detection"
        );
        assert!(
            captioned.contains("Check this out"),
            "caption text must be present in content"
        );

        // ── Markdown file sent as Photo────────────────
        let md_filename = "notes.md";
        let md_path = workspace.path().join(md_filename);
        std::fs::write(&md_path, b"# Hello\nSome markdown").expect("write md fixture");
        let md_content =
            format_attachment_content(IncomingAttachmentKind::Photo, md_filename, &md_path);
        assert!(
            !md_content.contains("[IMAGE:"),
            "markdown must not get [IMAGE:] marker: {md_content}"
        );
        let md_msgs = vec![zeroclaw_providers::ChatMessage::user(md_content)];
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&md_msgs),
            0,
            "markdown file must not trigger image marker detection"
        );
    }

    // ── Groq model_provider rejects photo with vision error ────────────────

    #[test]
    fn groq_provider_rejects_photo_with_vision_error() {
        use zeroclaw_providers::ModelProvider;
        use zeroclaw_providers::compatible::{AuthStyle, OpenAiCompatibleModelProvider};

        let groq = OpenAiCompatibleModelProvider::builder("test")
            .display_name("Groq")
            .base_url("https://api.groq.com/openai")
            .credential(Some("fake_key"))
            .auth_style(AuthStyle::Bearer)
            .build();

        // Groq must not support vision.
        assert!(
            !groq.supports_vision(),
            "Groq model_provider must not support vision"
        );

        // Build a message with an [IMAGE:] marker (as photo attachment would).
        let messages = vec![zeroclaw_providers::ChatMessage::user(
            "[IMAGE:/tmp/photo.jpg]\n\nDescribe this image".to_string(),
        )];
        let marker_count = zeroclaw_providers::multimodal::count_image_markers(&messages);
        assert_eq!(marker_count, 1, "must detect image marker in photo content");

        // The combination of marker_count > 0 && !supports_vision() means
        // the agent loop will return ProviderCapabilityError before calling
        // the model_provider, and the channel will send "⚠️ Error: ..." to the user.
    }

    #[test]
    fn ack_reactions_defaults_to_true() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        assert!(ch.ack_reactions);
    }

    #[test]
    fn with_ack_reactions_false_disables_reactions() {
        let mention_only = false;
        let ack_enabled = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_ack_reactions(ack_enabled);
        assert!(!ch.ack_reactions);
    }

    #[test]
    fn with_ack_reactions_true_keeps_reactions() {
        let mention_only = false;
        let ack_enabled = true;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_ack_reactions(ack_enabled);
        assert!(ch.ack_reactions);
    }

    // ── Forwarded message tests ─────────────────────────────────────

    #[test]
    fn format_forward_attribution_supports_forward_origin_variants() {
        let cases = vec![
            (
                "user with username",
                serde_json::json!({
                    "type": "user",
                    "sender_user": { "id": 123, "username": "alice" }
                }),
                "[Forwarded from @alice] ",
            ),
            (
                "user with display name",
                serde_json::json!({
                    "type": "user",
                    "sender_user": {
                        "id": 123,
                        "first_name": "Alice",
                        "last_name": "Smith"
                    }
                }),
                "[Forwarded from Alice Smith] ",
            ),
            (
                "hidden user",
                serde_json::json!({
                    "type": "hidden_user",
                    "sender_user_name": "Anonymous Sender"
                }),
                "[Forwarded from Anonymous Sender] ",
            ),
            (
                "chat",
                serde_json::json!({
                    "type": "chat",
                    "sender_chat": { "id": 123, "title": "Secret Group" }
                }),
                "[Forwarded from chat: Secret Group] ",
            ),
            (
                "channel",
                serde_json::json!({
                    "type": "channel",
                    "chat": { "id": 123, "title": "News Channel" }
                }),
                "[Forwarded from channel: News Channel] ",
            ),
        ];

        for (name, origin, expected) in cases {
            let message = serde_json::json!({ "forward_origin": origin });
            assert_eq!(
                TelegramChannel::format_forward_attribution(&message),
                Some(expected.to_string()),
                "{name}"
            );
        }
    }

    #[test]
    fn parse_update_message_forward_origin_variants_reach_channel_content() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );

        let cases = vec![
            (
                serde_json::json!({
                    "type": "user",
                    "sender_user": { "id": 123, "username": "bob" }
                }),
                "[Forwarded from @bob] forwarded item",
            ),
            (
                serde_json::json!({
                    "type": "hidden_user",
                    "sender_user_name": "Hidden User"
                }),
                "[Forwarded from Hidden User] forwarded item",
            ),
            (
                serde_json::json!({
                    "type": "chat",
                    "sender_chat": { "id": -123, "title": "Secret Group" }
                }),
                "[Forwarded from chat: Secret Group] forwarded item",
            ),
            (
                serde_json::json!({
                    "type": "channel",
                    "chat": { "id": 123, "title": "News Channel" }
                }),
                "[Forwarded from channel: News Channel] forwarded item",
            ),
        ];

        for (index, (origin, expected)) in cases.into_iter().enumerate() {
            let update = serde_json::json!({
                "update_id": 99 + index,
                "message": {
                    "message_id": 49 + index,
                    "text": "forwarded item",
                    "from": { "id": 1, "username": "alice" },
                    "chat": { "id": 999 },
                    "forward_origin": origin
                }
            });

            let msg = ch
                .parse_update_message(&update)
                .expect("forward_origin message should parse");
            assert_eq!(msg.content, expected);
        }
    }

    #[test]
    fn parse_update_message_forwarded_reply_keeps_quote_block_separate() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let update = serde_json::json!({
            "update_id": 110,
            "message": {
                "message_id": 60,
                "text": "look at this news",
                "from": { "id": 1, "username": "alice" },
                "chat": { "id": 999 },
                "forward_origin": {
                    "type": "channel",
                    "chat": { "id": 123, "title": "News Channel" }
                },
                "reply_to_message": {
                    "message_id": 59,
                    "text": "What do you think?",
                    "from": { "id": 2, "username": "bot" }
                }
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("forwarded reply should parse");
        assert_eq!(
            msg.content,
            "[Forwarded from channel: News Channel]\n\n> @bot:\n> What do you think?\n\nlook at this news"
        );
    }

    #[test]
    fn parse_update_message_forwarded_from_user_with_username() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let update = serde_json::json!({
            "update_id": 100,
            "message": {
                "message_id": 50,
                "text": "Check this out",
                "from": { "id": 1, "username": "alice" },
                "chat": { "id": 999 },
                "forward_from": {
                    "id": 42,
                    "first_name": "Bob",
                    "username": "bob"
                },
                "forward_date": 1_700_000_000
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("forwarded message should parse");
        assert_eq!(msg.content, "[Forwarded from @bob] Check this out");
    }

    #[test]
    fn parse_update_message_forwarded_from_channel() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let update = serde_json::json!({
            "update_id": 101,
            "message": {
                "message_id": 51,
                "text": "Breaking news",
                "from": { "id": 1, "username": "alice" },
                "chat": { "id": 999 },
                "forward_from_chat": {
                    "id": -1_001_234_567_890_i64,
                    "title": "Daily News",
                    "username": "dailynews",
                    "type": "channel"
                },
                "forward_date": 1_700_000_000
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("channel-forwarded message should parse");
        assert_eq!(
            msg.content,
            "[Forwarded from channel: Daily News] Breaking news"
        );
    }

    #[test]
    fn parse_update_message_forwarded_hidden_sender() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let update = serde_json::json!({
            "update_id": 102,
            "message": {
                "message_id": 52,
                "text": "Secret tip",
                "from": { "id": 1, "username": "alice" },
                "chat": { "id": 999 },
                "forward_sender_name": "Hidden User",
                "forward_date": 1_700_000_000
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("hidden-sender forwarded message should parse");
        assert_eq!(msg.content, "[Forwarded from Hidden User] Secret tip");
    }

    #[test]
    fn parse_update_message_non_forwarded_unaffected() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let update = serde_json::json!({
            "update_id": 103,
            "message": {
                "message_id": 53,
                "text": "Normal message",
                "from": { "id": 1, "username": "alice" },
                "chat": { "id": 999 }
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("non-forwarded message should parse");
        assert_eq!(msg.content, "Normal message");
    }

    #[test]
    fn parse_update_message_forwarded_from_user_no_username() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let update = serde_json::json!({
            "update_id": 104,
            "message": {
                "message_id": 54,
                "text": "Hello there",
                "from": { "id": 1, "username": "alice" },
                "chat": { "id": 999 },
                "forward_from": {
                    "id": 77,
                    "first_name": "Charlie"
                },
                "forward_date": 1_700_000_000
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("forwarded message without username should parse");
        assert_eq!(msg.content, "[Forwarded from Charlie] Hello there");
    }

    #[test]
    fn forwarded_photo_attachment_has_attribution() {
        // Verify that format_forward_attribution produces correct prefix
        // for a photo message (the actual download is async, so we test the
        // helper directly with a photo-bearing message structure).
        let message = serde_json::json!({
            "message_id": 60,
            "from": { "id": 1, "username": "alice" },
            "chat": { "id": 999 },
            "photo": [
                { "file_id": "abc123", "file_unique_id": "u1", "width": 320, "height": 240 }
            ],
            "forward_origin": {
                "type": "user",
                "sender_user": {
                    "id": 42,
                    "username": "bob"
                }
            },
            "forward_date": 1_700_000_000
        });

        let attr =
            TelegramChannel::format_forward_attribution(&message).expect("should detect forward");
        assert_eq!(attr, "[Forwarded from @bob] ");

        // Simulate what try_parse_attachment_message does after building content
        let photo_content = "[IMAGE:/tmp/photo.jpg]".to_string();
        let content = TelegramChannel::prepend_forward_attribution(&attr, photo_content);
        assert_eq!(content, "[Forwarded from @bob] [IMAGE:/tmp/photo.jpg]");
    }

    #[tokio::test]
    async fn register_bot_commands_sends_correct_payload() {
        use wiremock::matchers::{body_json, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        let expected_body = serde_json::json!({
            "commands": [
                { "command": "new",    "description": "Start a new conversation session" },
                { "command": "clear",  "description": "Clear this conversation session" },
                { "command": "stop",   "description": "Cancel the current in-flight task" },
                { "command": "model",  "description": "Show or switch the current model" },
                { "command": "models", "description": "List available model_providers or switch model_provider" },
                { "command": "config", "description": "Show current configuration" },
            ]
        });

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/setMyCommands$"))
            .and(body_json(&expected_body))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true, "result": true })),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_api_base(mock_server.uri());

        ch.register_bot_commands().await;

        // Mock expectation assert happens on MockServer drop
    }

    #[tokio::test]
    async fn register_bot_commands_handles_failure_gracefully() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/setMyCommands$"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                serde_json::json!({ "ok": false, "description": "Internal Server Error" }),
            ))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_api_base(mock_server.uri());

        // Should not panic — errors are logged, not propagated.
        ch.register_bot_commands().await;
    }

    #[test]
    fn sanitize_telegram_command_name_basic() {
        assert_eq!(sanitize_telegram_command_name("hello"), "hello");
        assert_eq!(sanitize_telegram_command_name("Hello"), "hello");
        assert_eq!(sanitize_telegram_command_name("my-skill"), "my_skill");
        assert_eq!(sanitize_telegram_command_name("my skill"), "my_skill");
        assert_eq!(
            sanitize_telegram_command_name("My Cool Skill!"),
            "my_cool_skill"
        );
    }

    #[test]
    fn sanitize_telegram_command_name_trims_underscores() {
        assert_eq!(sanitize_telegram_command_name("_leading"), "leading");
        assert_eq!(sanitize_telegram_command_name("trailing_"), "trailing");
        assert_eq!(sanitize_telegram_command_name("__both__"), "both");
    }

    #[test]
    fn sanitize_telegram_command_name_collapses_double_underscores() {
        assert_eq!(sanitize_telegram_command_name("a--b"), "a_b");
        assert_eq!(sanitize_telegram_command_name("a---b"), "a_b");
    }

    #[test]
    fn sanitize_telegram_command_name_truncates_to_32_chars() {
        let long = "a".repeat(50);
        let result = sanitize_telegram_command_name(&long);
        assert!(result.len() <= TELEGRAM_COMMAND_NAME_MAX_LEN);
        assert_eq!(result.len(), 32);
    }

    #[test]
    fn sanitize_telegram_command_name_empty_input() {
        assert_eq!(sanitize_telegram_command_name(""), "");
        assert_eq!(sanitize_telegram_command_name("---"), "");
    }

    #[test]
    fn truncate_telegram_command_description_short() {
        assert_eq!(
            truncate_telegram_command_description("Short desc"),
            "Short desc"
        );
    }

    #[test]
    fn truncate_telegram_command_description_at_limit() {
        let exact = "a".repeat(TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN);
        assert_eq!(truncate_telegram_command_description(&exact), exact);
    }

    #[test]
    fn truncate_telegram_command_description_over_limit() {
        let long = "a".repeat(TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN + 10);
        let result = truncate_telegram_command_description(&long);
        assert!(result.chars().count() <= TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn truncate_telegram_command_description_multibyte_within_char_limit() {
        let desc = format!("Multibyte weather description: {}", "🌧".repeat(30));
        assert!(desc.chars().count() <= TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN);
        assert!(desc.len() > TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN);
        let result = truncate_telegram_command_description(&desc);
        assert!(
            !result.ends_with('…'),
            "should not append ellipsis when within char limit"
        );
        assert_eq!(result, desc.trim());
    }

    #[tokio::test]
    async fn register_bot_commands_includes_skills() {
        use wiremock::matchers::{body_json, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let workspace = tempfile::tempdir().unwrap();
        let skill_dir = workspace.path().join("skills").join("weather");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: weather\ndescription: Check the weather forecast\n---\n# Weather\n",
        )
        .unwrap();

        let mock_server = MockServer::start().await;

        let expected_body = serde_json::json!({
            "commands": [
                { "command": "new",     "description": "Start a new conversation session" },
                { "command": "clear",   "description": "Clear this conversation session" },
                { "command": "stop",    "description": "Cancel the current in-flight task" },
                { "command": "model",   "description": "Show or switch the current model" },
                { "command": "models",  "description": "List available model_providers or switch model_provider" },
                { "command": "config",  "description": "Show current configuration" },
                { "command": "weather", "description": "Check the weather forecast" },
            ]
        });

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/setMyCommands$"))
            .and(body_json(&expected_body))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true, "result": true })),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_api_base(mock_server.uri())
        .with_workspace_dir(workspace.path().to_path_buf());

        ch.register_bot_commands().await;
    }

    #[tokio::test]
    async fn register_bot_commands_includes_tools_from_config() {
        use wiremock::matchers::{body_json, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        let expected_body = serde_json::json!({
            "commands": [
                { "command": "new",       "description": "Start a new conversation session" },
                { "command": "clear",     "description": "Clear this conversation session" },
                { "command": "stop",      "description": "Cancel the current in-flight task" },
                { "command": "model",     "description": "Show or switch the current model" },
                { "command": "models",    "description": "List available model_providers or switch model_provider" },
                { "command": "config",    "description": "Show current configuration" },
                { "command": "test_tool", "description": "A test tool" },
            ]
        });

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/setMyCommands$"))
            .and(body_json(&expected_body))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true, "result": true })),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let specs = vec![("test_tool".to_string(), "A test tool".to_string())];
        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_api_base(mock_server.uri())
        .with_tool_command_specs(specs);

        ch.register_bot_commands().await;
    }

    // ── Approval inline keyboard tests ────────────────────────

    #[test]
    fn pending_approvals_map_is_initially_empty() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let map = ch.pending_approvals.lock().await;
            assert!(map.is_empty());
        });
    }

    #[test]
    fn approval_timeout_defaults_to_120_and_is_overridable() {
        let mention_only = false;
        let ch = TelegramChannel::new(
            "t".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        assert_eq!(ch.approval_timeout_secs, 120);
        let ch = ch.with_approval_timeout_secs(30);
        assert_eq!(ch.approval_timeout_secs, 30);
    }

    #[tokio::test]
    async fn pending_approval_oneshot_delivers_response() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let mention_only = false;
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );
        let approval_id = "test-approval-123".to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        ch.pending_approvals
            .lock()
            .await
            .insert(approval_id.clone(), tx);

        // Simulate what listen() does when a callback_query arrives
        if let Some(sender) = ch.pending_approvals.lock().await.remove(&approval_id) {
            sender.send(ChannelApprovalResponse::Approve).unwrap();
        }

        let result = rx.await.unwrap();
        assert_eq!(result, ChannelApprovalResponse::Approve);
    }

    #[test]
    fn callback_data_format_parses_correctly() {
        // Verify the callback_data format used by request_approval
        let cb_data = "approval:abc-123:approve";
        let rest = cb_data.strip_prefix("approval:").unwrap();
        let (id, action) = rest.rsplit_once(':').unwrap();
        assert_eq!(id, "abc-123");
        assert_eq!(action, "approve");

        let cb_data = "approval:abc-123:deny";
        let rest = cb_data.strip_prefix("approval:").unwrap();
        let (id, action) = rest.rsplit_once(':').unwrap();
        assert_eq!(id, "abc-123");
        assert_eq!(action, "deny");

        let cb_data = "approval:abc-123:always";
        let rest = cb_data.strip_prefix("approval:").unwrap();
        let (id, action) = rest.rsplit_once(':').unwrap();
        assert_eq!(id, "abc-123");
        assert_eq!(action, "always");
    }

    #[test]
    fn callback_data_with_uuid_parses_correctly() {
        // UUIDs contain hyphens — rsplit_once(':') must split at the LAST colon
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let cb_data = format!("approval:{uuid}:approve");
        let rest = cb_data.strip_prefix("approval:").unwrap();
        let (id, action) = rest.rsplit_once(':').unwrap();
        assert_eq!(id, uuid);
        assert_eq!(action, "approve");
    }

    #[test]
    fn non_approval_callback_data_is_ignored() {
        let cb_data = "some_other_action:data";
        assert!(cb_data.strip_prefix("approval:").is_none());
    }
}
