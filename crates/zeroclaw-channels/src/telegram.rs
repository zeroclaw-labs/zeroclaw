use anyhow::Context;
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use reqwest::multipart::{Form, Part};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, ProgressEvent, SendMessage};
use zeroclaw_config::schema::{Config, StreamMode, TELEGRAM_OFFICIAL_API_BASE_URL};
use zeroclaw_runtime::i18n;
use zeroclaw_runtime::security::pairing::PairingGuard;

/// Telegram's maximum message length for text messages
const TELEGRAM_MAX_MESSAGE_LENGTH: usize = 4096;
const TELEGRAM_CONTINUED_PREFIX: &str = "(continued)\n\n";
const TELEGRAM_CONTINUES_SUFFIX: &str = "\n\n(continues...)";
const TELEGRAM_FENCE_REOPEN: &str = "```\n";
const TELEGRAM_FENCE_CLOSE: &str = "```";
const TELEGRAM_ACK_REACTIONS: &[&str] = &["⚡️", "👌", "👀", "🔥", "👍"];

/// Metadata for an incoming document or photo attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IncomingAttachment {
    file_id: String,
    file_name: Option<String>,
    file_size: Option<u64>,
    caption: Option<String>,
    /// Sender-declared MIME type (documents only; Telegram photos carry none).
    mime_type: Option<String>,
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
/// setMyCommands also refuses on total BODY SIZE, and reports that refusal with the
/// same `BOT_COMMANDS_TOO_MUCH` string it uses for too many commands. The size limit
/// is undocumented, so it is measured: against the live API, a body of 100 commands
/// serializing to 9,714 bytes is accepted and the same shape at 9,814 bytes is
/// refused. 100 commands each carrying a description at
/// `TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN` serializes to roughly 14,800 bytes, so the
/// two per-item caps can both be satisfied and the request still be rejected. Budget
/// well under the measured edge.
const TELEGRAM_MAX_BOT_COMMANDS_BODY_BYTES: usize = 8192;
/// Telegram command names: 1-32 lowercase a-z, 0-9, and underscore.
const TELEGRAM_COMMAND_NAME_MAX_LEN: usize = 32;
/// Telegram command descriptions nominally allow up to 256 characters per the API docs,
/// but empirical testing shows the API returns errors for descriptions substantially
/// longer than 100 characters. This conservative cap avoids that in practice.
const TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN: usize = 100;

/// Resolve a localized CLI string by Fluent key, using the process-global active locale.
fn telegram_cli_string(key: &str) -> String {
    i18n::get_required_cli_string(key)
}

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

/// Serialized length of the `setMyCommands` request body for `commands`.
///
/// A serialization failure cannot make the real body smaller, so it reports 0 and
/// lets the request itself surface the problem rather than dropping every command.
fn telegram_bot_commands_body_len(commands: &[serde_json::Value]) -> usize {
    serde_json::to_string(&serde_json::json!({ "commands": commands })).map_or(0, |s| s.len())
}

/// Drop trailing commands until the `setMyCommands` body fits
/// `TELEGRAM_MAX_BOT_COMMANDS_BODY_BYTES`, and report how many were dropped.
///
/// This is a second cap, applied after the count cap: the two are independent, and
/// a command set inside the count limit can still exceed the size limit.
fn fit_telegram_bot_commands_to_body_budget(commands: &mut Vec<serde_json::Value>) -> usize {
    let mut dropped = 0;
    while !commands.is_empty()
        && telegram_bot_commands_body_len(commands) > TELEGRAM_MAX_BOT_COMMANDS_BODY_BYTES
    {
        commands.pop();
        dropped += 1;
    }
    dropped
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

/// Build the user-facing content string for an incoming attachment.
///
/// An attachment earns the `[IMAGE:/path]` marker when the multimodal loader
/// will actually accept it (`provider_loadable_image_mime()`), regardless of
/// whether Telegram delivered it as a photo or a document — so an image sent
/// "as file", even extensionless, is still marked as an image rather than
/// falling back to the `[Document: name] /path` form.
///
/// The check is deliberately the *loadable* one rather than the conservative
/// `looks_like_image()`. A marker the loader rejects is worse than no marker:
/// preparation drops it in favour of a "could not be loaded" note, and the
/// `[Document: ...]` line that would have kept the saved path reachable was
/// never emitted. Formats outside the provider's set therefore stay documents,
/// which leaves both the bytes and a usable path in the model's hands.
/// The disposition Telegram commits to for an inbound attachment, resolved once
/// against the provider's loadability contract.
///
/// `parse_attachment_metadata` only yields documents and photos, so an
/// attachment is either a loadable image the provider will accept or a
/// document. The rendered text and the typed envelope both read this one
/// verdict, so a document the loader would reject cannot be re-decided as an
/// image by a later payload-only classifier.
fn attachment_marker_kind(
    attachment: &zeroclaw_api::media::MediaAttachment,
) -> zeroclaw_api::media::MarkerKind {
    if attachment.provider_loadable_image_mime().is_some() {
        zeroclaw_api::media::MarkerKind::Image
    } else {
        zeroclaw_api::media::MarkerKind::Document
    }
}

fn format_attachment_content(
    attachment: &zeroclaw_api::media::MediaAttachment,
    local_path: &Path,
) -> String {
    match attachment_marker_kind(attachment) {
        zeroclaw_api::media::MarkerKind::Image => format!("[IMAGE:{}]", local_path.display()),
        _ => format!(
            "[Document: {}] {}",
            attachment.file_name,
            local_path.display()
        ),
    }
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
    /// pending approval requests: callback_data key → pending approval
    pending_approvals: Arc<tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>>,
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

/// a tool approval awaiting an inline-keyboard tap
struct PendingApproval {
    sender: tokio::sync::oneshot::Sender<zeroclaw_api::channel::ChannelApprovalResponse>,
    tool_name: String,
}

/// Outcome of attempting to parse a single incoming Telegram update.
///
/// ⚠️ This enum is a *parser* outcome, not the acknowledgement source of
/// truth. `SkipPermanent` is overloaded: it means both "this parser does not
/// apply, try the next one" and "this update is genuinely, permanently
/// handled" (unauthorized sender, mention gate, duration/size limits, missing
/// config). Those two meanings are only safe to conflate because the parser
/// chain is mutually exclusive and attempted in a fixed order (text → voice →
/// attachment), so a `SkipPermanent` that falls out of the *last* parser is
/// always a genuine permanent skip.
///
/// Whether an update is acknowledged is therefore decided by
/// [`TelegramChannel::process_update`] together with [`UpdateOutcome`] — read
/// those two to reason about offset advancement, not this enum alone.
/// `RetryTransient` is reserved for fallible I/O (file download,
/// transcription, disk writes) so the caller can leave the update
/// unacknowledged and retry it on the next poll instead of silently dropping
/// it.
// `pub(crate)` so orchestrator regressions can receive the disposition returned
// by `try_parse_attachment_message` and unwrap the parsed message.
pub(crate) enum UpdateDisposition {
    // Boxed: `ChannelMessage` is far larger than the unit variants, and this
    // enum is constructed on every incoming update regardless of outcome.
    Parsed(Box<ChannelMessage>),
    SkipPermanent,
    RetryTransient,
}

/// Result of routing a single update through [`TelegramChannel::process_update`].
///
/// Both the startup/restart probe and the main long-poll loop drive their
/// batches of updates through the same per-update path so a queued update
/// seen at startup gets exactly the same offset-advance discipline as one
/// seen mid-run: the offset only moves past an update once it has been
/// delivered or permanently skipped, never while a transient failure or a
/// dropped receiver could still cause it to be lost.
enum UpdateOutcome {
    /// The update was delivered or permanently skipped; the offset has been
    /// advanced past it and the caller should keep processing the batch.
    Advanced,
    /// A transient failure occurred. The caller should stop processing the
    /// rest of this batch so the next poll retries starting at the
    /// still-unadvanced offset.
    StopBatch,
    /// The channel receiver has been dropped; the whole listen loop must
    /// exit immediately.
    ReceiverClosed,
}

/// Why a Telegram `getFile` lookup failed, classified for retry purposes.
///
/// The offset repair in this PR only helps if a failure that can never
/// succeed is distinguished from one that can. Telegram answers an invalid or
/// expired `file_id` with `200 OK` and an `ok: false` envelope carrying
/// `error_code: 400`; treating that as transient head-of-line blocks every
/// later update indefinitely, because the offset never advances past an
/// update whose download will never succeed.
///
/// Classification is deliberately conservative: only a confidently permanent
/// vendor rejection is `Permanent`. It requires structured evidence from the
/// Bot API itself — `ok: false` *and* an `error_code` on an explicit
/// allowlist of terminal conditions this implementation can substantiate.
/// Every other response — 5xx, 429, 408, state-dependent or unrecognised 4xx
/// codes, transport errors, malformed bodies, body-less non-2xx responses —
/// stays `Transient`, because retrying a recoverable failure is safe while
/// skipping a recoverable one loses a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileLookupFailure {
    /// Retrying may succeed: 5xx, 429, 408, any 4xx outside the terminal
    /// allowlist, transport failure, malformed, body-less, or otherwise
    /// unrecognised response.
    Transient,
    /// Retrying can never succeed: an explicit `ok: false` carrying an
    /// `error_code` on the terminal allowlist (invalid/expired file id, file
    /// too big, forbidden). Safe to acknowledge and move past.
    Permanent,
}

/// A `getFile` failure with the vendor diagnostics preserved.
///
/// The previous code mapped every failure to a single generic
/// "missing file_path in response" string, discarding Telegram's
/// `error_code` and `description` — the exact evidence an operator needs to
/// tell an expired file id from an outage.
#[derive(Debug)]
pub(crate) struct FileLookupError {
    pub(crate) kind: FileLookupFailure,
    pub(crate) message: String,
}

impl std::fmt::Display for FileLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl FileLookupError {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            kind: FileLookupFailure::Transient,
            message: message.into(),
        }
    }

    fn permanent(message: impl Into<String>) -> Self {
        Self {
            kind: FileLookupFailure::Permanent,
            message: message.into(),
        }
    }

    /// Map a `getFile` response onto a classified failure.
    ///
    /// `status` is the HTTP status; `body` is the parsed JSON envelope when
    /// one could be parsed. Telegram returns errors both as non-2xx statuses
    /// and as `200 OK` with `ok: false`, so both shapes are inspected.
    ///
    /// Only `error_code` values on an explicit allowlist of substantiated
    /// terminal conditions are `Permanent`; every other structured code is
    /// left `Transient` so an uncommon or future recoverable rejection can
    /// never silently consume the update.
    pub(crate) fn classify(status: reqwest::StatusCode, body: Option<&serde_json::Value>) -> Self {
        let ok_flag = body
            .and_then(|b| b.get("ok"))
            .and_then(serde_json::Value::as_bool);
        let error_code = body
            .and_then(|b| b.get("error_code"))
            .and_then(serde_json::Value::as_i64);
        let description = body
            .and_then(|b| b.get("description"))
            .and_then(serde_json::Value::as_str);

        let detail = format!(
            "Telegram getFile failed (http {}, error_code {}, ok {}): {}",
            status.as_u16(),
            error_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string()),
            ok_flag
                .map(|b| b.to_string())
                .unwrap_or_else(|| "-".to_string()),
            description.unwrap_or("no description"),
        );

        // The only `error_code` values this implementation can substantiate
        // as terminal for `getFile`:
        //
        // * 400 Bad Request — invalid, expired, or malformed `file_id`, and
        //   "file is too big". The same `file_id` can never resolve later.
        // * 403 Forbidden — the bot lost access to the file's chat. Retrying
        //   the lookup with the same credentials cannot regain it.
        //
        // Everything else stays transient *by construction*. A 4xx status
        // does not prove permanence: HTTP defines state-dependent and
        // explicitly retryable 4xx conditions (409 Conflict, 425 Too Early),
        // Telegram documents `error_code` contents as subject to change, and
        // a future or uncommon code could well be recoverable. Codes that are
        // global rather than per-update — 401 (bad token), 404 (unknown
        // method) — are also left transient: they resolve when an operator
        // fixes the deployment, and acknowledging updates in the meantime
        // would discard them permanently.
        const TERMINAL_ERROR_CODES: [i64; 2] = [400, 403];

        // Permanence requires *structured vendor evidence* of a terminal
        // rejection: Telegram must both mark the call failed (`ok: false`)
        // and name a reason on the allowlist above. A bare HTTP status is not
        // enough — a body-less or malformed 4xx can come from an
        // intermediary rather than the Bot API. Guessing permanence there
        // would acknowledge and discard the update this path exists to
        // preserve, so anything unrecognised stays transient and is retried
        // instead.
        let terminal_rejection = ok_flag == Some(false)
            && error_code
                .map(|c| TERMINAL_ERROR_CODES.contains(&c))
                .unwrap_or(false);

        if terminal_rejection {
            Self::permanent(detail)
        } else {
            Self::transient(detail)
        }
    }
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
            serde_json::json!({ "command": "new",    "description": telegram_cli_string("channel-telegram-cmd-new-desc") }),
            serde_json::json!({ "command": "clear",  "description": telegram_cli_string("channel-telegram-cmd-clear-desc") }),
            serde_json::json!({ "command": "stop",   "description": telegram_cli_string("channel-telegram-cmd-stop-desc") }),
            serde_json::json!({ "command": "model",  "description": telegram_cli_string("channel-telegram-cmd-model-desc") }),
            serde_json::json!({ "command": "models", "description": telegram_cli_string("channel-telegram-cmd-models-desc") }),
            serde_json::json!({ "command": "config", "description": telegram_cli_string("channel-telegram-cmd-config-desc") }),
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
            let registered = commands.len();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "TELEGRAM_MAX_BOT_COMMANDS": TELEGRAM_MAX_BOT_COMMANDS,
                        "total_before_cap": total_before_cap,
                        "registered": registered,
                    })),
                // Stable literal per the logging contract: per-event
                // measurements (limit, configured, registered) ride solely in
                // `attributes` above, never in the message.
                "Telegram command registration truncated to the platform limit"
            );
        }

        // Second, independent cap: setMyCommands also refuses on total body size,
        // reporting it as BOT_COMMANDS_TOO_MUCH. A set inside the count limit can
        // still exceed it, which is why this runs after the truncation above.
        let before_body_cap = commands.len();
        let dropped_for_body = fit_telegram_bot_commands_to_body_budget(&mut commands);
        if dropped_for_body > 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "TELEGRAM_MAX_BOT_COMMANDS_BODY_BYTES":
                            TELEGRAM_MAX_BOT_COMMANDS_BODY_BYTES,
                        "before_body_cap": before_body_cap,
                        "registered": commands.len(),
                        "dropped": dropped_for_body,
                    })),
                // Stable literal per the logging contract: per-event measurements
                // ride solely in `attributes` above, never in the message.
                "Telegram command registration trimmed to the platform body-size limit"
            );
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
        let Some(tts_manager) = self.tts_manager.clone() else {
            return;
        };

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
        let status = resp.status();
        if status.is_success() {
            // the Bot API envelope is the outcome: only an explicit boolean
            // ok:true makes a 2xx a successful edit. A truncated body, a
            // proxy-generated 200, a missing or non-boolean ok field would
            // otherwise silently report a card rewrite that never happened.
            let bytes = resp.bytes().await.unwrap_or_default();
            let envelope = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
            let ok = envelope
                .as_ref()
                .and_then(|v| v.get("ok").and_then(|b| b.as_bool()));
            match ok {
                Some(true) => return EditMessageResult::Success,
                Some(false) => {
                    let description = envelope
                        .as_ref()
                        .and_then(|v| v.get("description"))
                        .and_then(|d| d.as_str())
                        .unwrap_or_default();
                    if description.contains("message is not modified") {
                        return EditMessageResult::NotModified;
                    }
                    return EditMessageResult::Failed(status);
                }
                // missing field, wrong type, unparseable body, read failure
                None => return EditMessageResult::Failed(status),
            }
        }

        let body = resp.text().await.unwrap_or_default();
        if body.contains("message is not modified") {
            return EditMessageResult::NotModified;
        }

        EditMessageResult::Failed(status)
    }

    /// Shortest window a deadline-firing waiter waits for a callback that
    /// claimed the resolution first. The callback sends immediately after
    /// claiming, so this only needs to cover the task-scheduling gap.
    const CALLBACK_CLAIM_GRACE: Duration = Duration::from_secs(5);

    /// Decide the approval outcome after the wait deadline has fired. The
    /// pending entry is the single resolution claim: winning its removal
    /// makes the runtime's timed-out deny the outcome, while losing the
    /// claim means a callback claimed first and its response is in flight
    /// on the channel, so it is consumed here instead of overridden.
    async fn resolve_after_deadline(
        &self,
        approval_id: &str,
        rx: &mut tokio::sync::oneshot::Receiver<zeroclaw_api::channel::ChannelApprovalResponse>,
    ) -> zeroclaw_api::channel::AttributedApprovalResponse {
        let claimed = self
            .pending_approvals
            .lock()
            .await
            .remove(approval_id)
            .is_some();
        if claimed {
            return zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                zeroclaw_api::channel::ChannelApprovalResponse::Deny,
                zeroclaw_api::channel::ApprovalSource::TimedOut,
            );
        }
        match tokio::time::timeout(Self::CALLBACK_CLAIM_GRACE, rx).await {
            Ok(Ok(response)) => {
                zeroclaw_api::channel::AttributedApprovalResponse::operator(response)
            }
            _ => zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                zeroclaw_api::channel::ChannelApprovalResponse::Deny,
                zeroclaw_api::channel::ApprovalSource::TimedOut,
            ),
        }
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

    /// True when `message` carries content one of the update parsers
    /// would actually process for an authorized sender.
    ///
    /// Acceptance is resolved from the canonical typed parsers rather
    /// than from raw JSON key presence, so this predicate cannot drift
    /// from what the parsers accept: `text` must deserialize as a string
    /// (`parse_update_message`), a `voice`/`audio` payload must yield
    /// metadata via `parse_voice_metadata`, and a `document`/`photo`
    /// payload must yield an `IncomingAttachment` via
    /// `parse_attachment_metadata` (which rejects a missing/non-string
    /// `file_id` and an empty `photo` array). Telegram response JSON is
    /// an external trust boundary, so its shape is validated before any
    /// behavior — including the approval notice — is triggered.
    ///
    /// The live config gates are retained alongside the typed checks:
    /// voice/audio only counts when the transcription config and manager
    /// `try_parse_voice_message` requires are both present, and
    /// document/photo only when the workspace dir
    /// `try_parse_attachment_message` downloads into is set.
    ///
    /// This covers the config-shaped and shape-shaped bails; the
    /// content-shaped permanent bails — over-`max_duration_secs`
    /// voice/audio and over-`TELEGRAM_MAX_FILE_DOWNLOAD_BYTES`
    /// attachments — are checked separately by
    /// `message_exceeds_parser_limits`, kept out of this predicate
    /// specifically so a captioned `/bind <code>` still reaches the
    /// pairing branch in `handle_unauthorized_message` even on media
    /// those limits would otherwise reject.
    fn message_has_processable_content(&self, message: &serde_json::Value) -> bool {
        if message
            .get("text")
            .and_then(serde_json::Value::as_str)
            .is_some()
        {
            return true;
        }
        if self.transcription.is_some()
            && self.transcription_manager.is_some()
            && Self::parse_voice_metadata(message).is_some()
        {
            return true;
        }
        self.workspace_dir.is_some() && Self::parse_attachment_metadata(message).is_some()
    }

    /// True when `message` is a voice/audio or document/photo update that
    /// one of the update parsers would permanently bail on for size or
    /// duration alone, independent of authorization — mirroring
    /// `try_parse_voice_message`'s over-`max_duration_secs` bail (using
    /// the same `self.transcription` config the parser reads) and
    /// `try_parse_attachment_message`'s over-`TELEGRAM_MAX_FILE_DOWNLOAD_BYTES`
    /// bail (the same constant the parser checks). An authorized sender's
    /// identical update would be silently dropped for this reason, so an
    /// unauthorized sender must not receive the approval notice for it
    /// either. Deliberately excluded from `message_has_processable_content`
    /// so the captioned `/bind <code>` pairing path in
    /// `handle_unauthorized_message` — checked before this — is not
    /// gated by it.
    fn message_exceeds_parser_limits(&self, message: &serde_json::Value) -> bool {
        if let Some((_, duration)) = Self::parse_voice_metadata(message)
            && let Some(config) = self.transcription.as_ref()
            && duration > config.max_duration_secs
        {
            return true;
        }
        if let Some(attachment) = Self::parse_attachment_metadata(message)
            && let Some(size) = attachment.file_size
            && size > TELEGRAM_MAX_FILE_DOWNLOAD_BYTES
        {
            return true;
        }
        false
    }

    async fn handle_unauthorized_message(&self, update: &serde_json::Value) {
        let Some(message) = update.get("message") else {
            return;
        };

        // Only updates an authorized sender would have gotten processed
        // deserve the unauthorized notice. Everything else — service
        // messages (joins/leaves/pins), stickers, locations, contacts,
        // or media this deployment is not configured to process — stays
        // silent exactly as before; a notice would either spam group
        // chats on every join, or promise processing that can never
        // happen.
        if !self.message_has_processable_content(message) {
            return;
        }

        // Media updates carry no top-level `text`, only an optional
        // `caption`; fall back to it so a captioned `/bind <code>` still
        // reaches the pairing flow below. Captionless media yields "",
        // which simply finds no bind code and falls through to the
        // unauthorized-approval notice — the same outcome unauthorized
        // text senders already get.
        let text = message
            .get("text")
            .and_then(serde_json::Value::as_str)
            .or_else(|| message.get("caption").and_then(serde_json::Value::as_str))
            .unwrap_or("");

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

        // No bind code — this is heading for the approval notice. Bail
        // silently here if the parsers would have permanently rejected
        // this exact update on size/duration alone: an authorized
        // sender's identical voice/attachment would be dropped for the
        // same reason, so the notice must not promise processing that
        // can never happen.
        if self.message_exceeds_parser_limits(message) {
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
    ///
    /// Failures carry the vendor's HTTP status, `ok` flag, `error_code`, and
    /// `description`, classified as [`FileLookupFailure::Permanent`] or
    /// `Transient` so the caller can acknowledge an update whose download can
    /// never succeed instead of retrying it forever.
    async fn get_file_path(&self, file_id: &str) -> Result<String, FileLookupError> {
        let url = self.api_url("getFile");
        let resp = match self
            .http_client()
            .get(&url)
            .query(&[("file_id", file_id)])
            .send()
            .await
        {
            Ok(r) => r,
            // A transport failure says nothing about the file id.
            Err(e) => {
                return Err(FileLookupError::transient(format!(
                    "Failed to call Telegram getFile: {e}"
                )));
            }
        };

        let status = resp.status();
        let body: Option<serde_json::Value> = resp.json().await.ok();

        // The happy path: a usable file_path regardless of envelope noise.
        if let Some(path) = body
            .as_ref()
            .and_then(|b| b.get("result"))
            .and_then(|r| r.get("file_path"))
            .and_then(serde_json::Value::as_str)
        {
            return Ok(path.to_string());
        }

        Err(FileLookupError::classify(status, body.as_ref()))
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
            let mime_type = doc
                .get("mime_type")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            let caption = message
                .get("caption")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            return Some(IncomingAttachment {
                file_id,
                file_name,
                file_size,
                caption,
                mime_type,
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
                mime_type: None,
                kind: IncomingAttachmentKind::Photo,
            });
        }

        None
    }

    /// Attempt to parse a Telegram update as a document/photo attachment.
    ///
    /// Downloads the file to `{workspace_dir}/telegram_files/` and returns a
    /// `Parsed` disposition carrying a `ChannelMessage` with the local file
    /// path, `SkipPermanent` when the update is not a parseable attachment, or
    /// `RetryTransient` when a download or write fails and is worth retrying.
    ///
    /// `pub(crate)` so orchestrator regressions can drive a REAL parsed
    /// Telegram update through `process_channel_message` (the live
    /// smoke failed precisely in the seam between this parser and the
    /// orchestrator's typed image gate).
    pub(crate) async fn try_parse_attachment_message(
        &self,
        update: &serde_json::Value,
    ) -> UpdateDisposition {
        let Some(message) = update.get("message") else {
            return UpdateDisposition::SkipPermanent;
        };
        let Some(attachment) = Self::parse_attachment_metadata(message) else {
            return UpdateDisposition::SkipPermanent;
        };

        // Check file size limit
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
            return UpdateDisposition::SkipPermanent;
        }

        let (username, sender_id, sender_identity) = Self::extract_sender_info(message);

        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }

        if !self.is_any_user_allowed(identities.iter().copied()) {
            return UpdateDisposition::SkipPermanent;
        }

        // Apply mention_only gate before downloading. Photo / document
        // updates carry no `text` field, so the text-only gate in
        // `parse_update_message` can never see them and they used to slip
        // through unconditionally.
        let Some(gated_caption) =
            self.check_media_mention_gate(message, attachment.caption.as_deref())
        else {
            return UpdateDisposition::SkipPermanent;
        };

        let Some(chat_id) = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string())
        else {
            return UpdateDisposition::SkipPermanent;
        };

        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        let thread_id = Self::topic_thread_id(message);

        let reply_target = if let Some(ref tid) = thread_id {
            format!("{}:{}", chat_id, tid)
        } else {
            chat_id.clone()
        };

        // Ensure workspace directory is configured
        let Some(workspace) = self.workspace_dir.as_ref().or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Cannot save attachment: workspace_dir not configured"
            );
            None
        }) else {
            return UpdateDisposition::SkipPermanent;
        };

        let save_dir = workspace.join("telegram_files");
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                "Failed to create telegram_files directory"
            );
            return UpdateDisposition::RetryTransient;
        }

        // Download file from Telegram
        let tg_file_path = match self.get_file_path(&attachment.file_id).await {
            Ok(p) => p,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "error": zeroclaw_runtime::security::scrub(&format!("{}", e)),
                            "classification": format!("{:?}", e.kind),
                        })),
                    "Failed to get attachment file path"
                );
                // A permanently rejected file id can never download; retrying
                // it head-of-line blocks every later update forever.
                return match e.kind {
                    FileLookupFailure::Permanent => UpdateDisposition::SkipPermanent,
                    FileLookupFailure::Transient => UpdateDisposition::RetryTransient,
                };
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
                return UpdateDisposition::RetryTransient;
            }
        };

        // Determine local filename
        let local_filename = match &attachment.file_name {
            Some(name) => name.clone(),
            None => {
                // For photos, derive extension from Telegram file path
                let ext = tg_file_path.rsplit('.').next().unwrap_or("jpg");
                format!("photo_{chat_id}_{message_id}.{ext}")
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
            return UpdateDisposition::RetryTransient;
        }

        // Carry a typed envelope alongside the content marker (parity with
        // Discord's documented attachment contract). Message text is a
        // rendering, not a source of truth: any consumer that needs to know
        // whether a turn carried an image must be able to ask
        // `msg.attachments` and get a truthful answer, so leaving the envelope
        // empty here would make a real photo turn indistinguishable from a
        // text one. Applies to documents too, with the sender's declared MIME
        // carried through: `looks_like_image()` classifies by MIME, extension,
        // or magic bytes, so an image sent "as file" (even extensionless) is
        // still reported as an image.
        let mut media_attachment = zeroclaw_api::media::MediaAttachment {
            file_name: local_filename.clone(),
            data: file_data,
            mime_type: attachment.mime_type.clone(),
            marker: None,
        };

        // Record the disposition this channel commits to together with the
        // saved path it references, resolved once against the loadability
        // contract. The rendering below reads the same verdict, so an
        // unsupported image document stays a document end to end: the pipeline
        // reads `marker` and defers instead of re-classifying the bytes as an
        // image and inlining a base64 copy the provider would reject.
        let marker_kind = attachment_marker_kind(&media_attachment);
        media_attachment.marker = Some(zeroclaw_api::media::RenderedMarker {
            target: local_path.display().to_string(),
            kind: marker_kind,
        });

        // Build message content. The marker is decided by the envelope's
        // loadable-image verdict, not Telegram's photo/document
        // classification, so image documents get the same re-loadable
        // [IMAGE:] marker as photos and the media pipeline can recognize
        // them as already-marked instead of re-inlining base64.
        let mut content = format_attachment_content(&media_attachment, &local_path);
        // `gated_caption` is the trimmed caption when the `mention_only`
        // gate admits it; otherwise the raw caption (or None).
        if let Some(caption) = gated_caption.as_deref()
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

        UpdateDisposition::Parsed(Box::new(ChannelMessage {
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
            attachments: vec![media_attachment],
            subject: None,

            ..Default::default()
        }))
    }

    /// Attempt to parse a Telegram update as a voice message and transcribe it.
    /// Returns `SkipPermanent` if the message is not a voice message, transcription is
    /// disabled, or the message exceeds duration limits; `RetryTransient` if download or
    /// transcription I/O fails.
    async fn try_parse_voice_message(&self, update: &serde_json::Value) -> UpdateDisposition {
        let Some(config) = self.transcription.as_ref() else {
            return UpdateDisposition::SkipPermanent;
        };
        let Some(manager) = self.transcription_manager.as_deref() else {
            return UpdateDisposition::SkipPermanent;
        };
        let Some(message) = update.get("message") else {
            return UpdateDisposition::SkipPermanent;
        };

        let Some((file_id, duration)) = Self::parse_voice_metadata(message) else {
            return UpdateDisposition::SkipPermanent;
        };

        if duration > config.max_duration_secs {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Skipping voice message: duration {duration}s exceeds limit {}s",
                    config.max_duration_secs
                )
            );
            return UpdateDisposition::SkipPermanent;
        }

        let (username, sender_id, sender_identity) = Self::extract_sender_info(message);

        let mut identities = vec![username.as_str()];
        if let Some(id) = sender_id.as_deref() {
            identities.push(id);
        }

        if !self.is_any_user_allowed(identities.iter().copied()) {
            return UpdateDisposition::SkipPermanent;
        }

        let voice_caption = message.get("caption").and_then(serde_json::Value::as_str);
        if self
            .check_media_mention_gate(message, voice_caption)
            .is_none()
        {
            return UpdateDisposition::SkipPermanent;
        }

        let Some(chat_id) = message
            .get("chat")
            .and_then(|chat| chat.get("id"))
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string())
        else {
            return UpdateDisposition::SkipPermanent;
        };

        let message_id = message
            .get("message_id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        let thread_id = Self::topic_thread_id(message);

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
                        .with_attrs(::serde_json::json!({
                            "error": zeroclaw_runtime::security::scrub(&format!("{}", e)),
                            "classification": format!("{:?}", e.kind),
                        })),
                    "Failed to get voice file path"
                );
                // See the attachment path: a permanent vendor rejection must
                // not hold the offset, or the batch never drains.
                return match e.kind {
                    FileLookupFailure::Permanent => UpdateDisposition::SkipPermanent,
                    FileLookupFailure::Transient => UpdateDisposition::RetryTransient,
                };
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
                return UpdateDisposition::RetryTransient;
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
                return UpdateDisposition::RetryTransient;
            }
        };

        if text.trim().is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Voice transcription returned empty text, skipping"
            );
            return UpdateDisposition::SkipPermanent;
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

        UpdateDisposition::Parsed(Box::new(ChannelMessage {
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
        }))
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

    /// Forum-topic thread id for history keying and reply routing, if this
    /// message belongs to a genuine forum topic. Telegram also sets
    /// `message_thread_id` for ordinary reply-threads in supergroups, which are
    /// NOT topic boundaries and must continue the main chat's conversation
    /// history — so gate on `is_topic_message` and treat a non-topic thread as
    /// the main chat (no `thread_ts`, no `:tid` on `reply_target`).
    fn topic_thread_id(message: &serde_json::Value) -> Option<String> {
        if message
            .get("is_topic_message")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return None;
        }
        message
            .get("message_thread_id")
            .and_then(serde_json::Value::as_i64)
            .map(|id| id.to_string())
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
        let thread_id = Self::topic_thread_id(message);

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
                let Some(ch) = line[i..].chars().next() else {
                    break;
                };
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

    /// handle a tool-approval `callback_query`: resolve the pending approval,
    /// dismiss the spinner, and rewrite the prompt with the outcome, dropping
    /// the buttons
    /// best-effort rewrite, a stale tap is a no-op
    async fn handle_approval_callback(&self, cb: &serde_json::Value) {
        let cb_id = cb
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let cb_data = cb
            .get("data")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        let Some(rest) = cb_data.strip_prefix("approval:") else {
            return;
        };
        let Some((approval_id, action)) = rest.rsplit_once(':') else {
            return;
        };

        let response = match action {
            "approve" => Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve),
            "always" => Some(zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove),
            "deny" => Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny),
            other => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"other": other})),
                    "Unknown approval callback action"
                );
                None
            }
        };

        // The pending entry is the single resolution claim: removing it wins
        // the right to decide, and a won claim is only published when the
        // response actually reaches the waiter on the channel. A lost claim
        // or a failed send gets an honest already-resolved toast and no card
        // rewrite, so Telegram can never show an outcome the runtime did not
        // record.
        let has_response = response.is_some();
        let resolved_tool = if let Some(resp) = response {
            match self.pending_approvals.lock().await.remove(approval_id) {
                Some(pending) => match pending.sender.send(resp) {
                    Ok(()) => Some(pending.tool_name),
                    Err(_) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "approval_id":
                                    zeroclaw_runtime::security::scrub(approval_id)
                            })),
                            "approval callback lost the resolution race; card left untouched"
                        );
                        None
                    }
                },
                None => None,
            }
        } else {
            None
        };

        // dismiss the client spinner: a won claim acknowledges the tapped
        // action, anything else gets the already-resolved toast
        let answer_text = if resolved_tool.is_some() {
            match action {
                "approve" => format!(
                    "✅ {}",
                    i18n::get_required_cli_string("channel-telegram-approval-ack-approved")
                ),
                "always" => format!(
                    "✅✅ {}",
                    i18n::get_required_cli_string("channel-telegram-approval-ack-always-approved")
                ),
                "deny" => format!(
                    "❌ {}",
                    i18n::get_required_cli_string("channel-telegram-approval-ack-denied")
                ),
                _ => format!(
                    "⚠️ {}",
                    i18n::get_required_cli_string("channel-telegram-approval-ack-unknown")
                ),
            }
        } else if has_response {
            format!(
                "⏳ {}",
                i18n::get_required_cli_string("channel-telegram-approval-ack-already-resolved")
            )
        } else {
            format!(
                "⚠️ {}",
                i18n::get_required_cli_string("channel-telegram-approval-ack-unknown")
            )
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
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                "answerCallbackQuery failed"
            );
        }

        // rewrite the prompt in place, dropping the keyboard
        if let Some(tool_name) = resolved_tool {
            let chat_id = cb
                .get("message")
                .and_then(|m| m.get("chat"))
                .and_then(|c| c.get("id"))
                .and_then(serde_json::Value::as_i64);
            let message_id = cb
                .get("message")
                .and_then(|m| m.get("message_id"))
                .and_then(serde_json::Value::as_i64);
            let user_first_name = cb
                .get("from")
                .and_then(|f| f.get("first_name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Operator");

            if let (Some(chat_id), Some(message_id)) = (chat_id, message_id) {
                // an InlineKeyboardMarkup with zero rows is the Bot API's
                // documented way to drop the buttons; a bare {} is not a
                // valid markup object (inline_keyboard is a required field)
                let edit_body = serde_json::json!({
                    "chat_id": chat_id,
                    "message_id": message_id,
                    "text": format!("{} by {}: {}", answer_text, user_first_name, tool_name),
                    "reply_markup": { "inline_keyboard": [] },
                });
                match self
                    .http_client()
                    .post(self.api_url("editMessageText"))
                    .json(&edit_body)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if let EditMessageResult::Failed(status) =
                            Self::classify_edit_message_response(resp).await
                        {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"status": status.to_string()})),
                                "editMessageText (approval resolution) failed"
                            );
                        }
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{}", e))})),
                            "editMessageText (approval resolution) request failed"
                        );
                    }
                }
            }
        }
    }

    /// Fixed, bounded delay between retries of a transiently failing update.
    /// The attempt count is diagnostic only: only an explicitly permanent
    /// disposition may advance the Telegram offset.
    const TRANSIENT_RETRY_DELAY_SECS: u64 = 2;

    /// Route a single update from a `getUpdates` batch through the shared
    /// delivered/permanent-skip/retry-transient disposition path.
    ///
    /// This is called from both the startup/restart probe and the main
    /// long-poll loop so a queued update sitting in the probe's first batch
    /// is handled identically to one seen mid-run: `offset` only advances
    /// past an update once it has been delivered (`tx.send` succeeded) or
    /// permanently skipped, never while a transient failure or a dropped
    /// `tx` receiver could still cause it to be lost.
    async fn process_update(
        &self,
        update: &serde_json::Value,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        offset: &mut i64,
        transient_retry: &mut Option<(i64, u32)>,
    ) -> UpdateOutcome {
        let uid = update.get("update_id").and_then(serde_json::Value::as_i64);

        // ── Handle callback_query (inline keyboard taps) ──
        if let Some(cb) = update.get("callback_query") {
            self.handle_approval_callback(cb).await;

            // A callback_query is terminal for inbound processing: there is
            // no message to deliver downstream, so nothing can be lost by
            // acknowledging it. The spinner dismissal above is best-effort:
            // it does perform HTTP I/O and
            // its failure is logged, but a failed spinner dismissal must not
            // hold up the offset, since retrying the update would re-run the
            // approval side effect that has already been applied.
            if let Some(uid) = uid {
                *offset = uid + 1;
            }
            return UpdateOutcome::Advanced;
        }

        // `parse_update_message` handles text messages and has no fallible
        // I/O, so its `None` always means "not applicable", fall through to
        // the voice parser next. The voice and attachment parsers can
        // additionally fail transiently on download/transcription I/O; a
        // transient failure must abort this update's processing entirely
        // (not fall through to the next parser) so the offset stays put and
        // the next poll retries it.
        let disposition = if let Some(m) = self.parse_update_message(update) {
            UpdateDisposition::Parsed(Box::new(m))
        } else {
            match self.try_parse_voice_message(update).await {
                UpdateDisposition::SkipPermanent => self.try_parse_attachment_message(update).await,
                other => other,
            }
        };

        let msg = match disposition {
            UpdateDisposition::Parsed(m) => m,
            UpdateDisposition::SkipPermanent => {
                Box::pin(self.handle_unauthorized_message(update)).await;
                if let Some(uid) = uid {
                    *offset = uid + 1;
                    *transient_retry = None;
                }
                return UpdateOutcome::Advanced;
            }
            UpdateDisposition::RetryTransient => {
                let attempts = if let Some(uid) = uid {
                    let attempts = match *transient_retry {
                        Some((tracked_uid, n)) if tracked_uid == uid => n.saturating_add(1),
                        _ => 1,
                    };
                    *transient_retry = Some((uid, attempts));
                    attempts
                } else {
                    1
                };
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "update_id": uid,
                            "attempts": attempts,
                            "retry_delay_secs": Self::TRANSIENT_RETRY_DELAY_SECS,
                        })),
                    "Transient failure parsing update; leaving offset unadvanced so the next poll retries it"
                );
                tokio::time::sleep(std::time::Duration::from_secs(
                    Self::TRANSIENT_RETRY_DELAY_SECS,
                ))
                .await;
                return UpdateOutcome::StopBatch;
            }
        };

        if self.ack_reactions
            && let Some((reaction_chat_id, reaction_message_id)) =
                Self::extract_update_message_target(update)
        {
            self.try_add_ack_reaction_nonblocking(reaction_chat_id, reaction_message_id);
        }

        // Send "typing" indicator immediately when we receive a message
        let typing_body = serde_json::json!({
            "chat_id": &msg.reply_target,
            "action": "typing"
        });
        let _ = self
            .http_client()
            .post(self.api_url("sendChatAction"))
            .json(&typing_body)
            .send()
            .await; // Ignore errors for typing indicator

        match tx.send(*msg).await {
            Ok(()) => {
                if let Some(uid) = uid {
                    *offset = uid + 1;
                    *transient_retry = None;
                }
                UpdateOutcome::Advanced
            }
            Err(_) => UpdateOutcome::ReceiverClosed,
        }
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

    async fn update_draft_lifecycle(
        &self,
        recipient: &str,
        message_id: &str,
        event: ProgressEvent,
    ) -> anyhow::Result<()> {
        if self.stream_mode == StreamMode::Partial {
            let status_line = crate::util::localized_lifecycle_progress(event);
            return self.update_draft(recipient, message_id, &status_line).await;
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
        // Single-slot transient-retry tracker: (update_id, attempts so far).
        // One slot is sufficient because a transient failure via
        // `process_update` stops processing of the current update batch (be
        // it the startup probe's batch below or the main loop's), so at most
        // one update can be head-of-line blocking retries at any time. Once
        // the attempt count grows only for operator diagnostics. It never
        // changes the delivery disposition: an unclassified I/O failure
        // cannot become safe to acknowledge merely because it repeated.
        // Shared across both the startup probe and the main loop so a
        // transient failure on a queued startup update is tracked the same
        // way as one seen mid-run.
        let mut transient_retry: Option<(i64, u32)> = None;

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
                                // Slot claimed. Route any queued updates through the
                                // same delivered/permanent-skip/retry-transient
                                // disposition path as the main loop below, instead of
                                // blindly advancing the offset past them: a transient
                                // failure or a dropped receiver here must leave the
                                // offset unadvanced too, so the update survives until
                                // a later poll (in this probe or the main loop) can
                                // actually deliver it.
                                if let Some(results) =
                                    data.get("result").and_then(serde_json::Value::as_array)
                                {
                                    for update in results {
                                        match self
                                            .process_update(
                                                update,
                                                &tx,
                                                &mut offset,
                                                &mut transient_retry,
                                            )
                                            .await
                                        {
                                            UpdateOutcome::Advanced => {}
                                            UpdateOutcome::StopBatch => break,
                                            UpdateOutcome::ReceiverClosed => return Ok(()),
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
            let body = serde_json::json!({
                "offset": offset,
                "timeout": 30,
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

            if let Some(results) = data.get("result").and_then(serde_json::Value::as_array) {
                for update in results {
                    match self
                        .process_update(update, &tx, &mut offset, &mut transient_retry)
                        .await
                    {
                        UpdateOutcome::Advanced => {}
                        UpdateOutcome::StopBatch => break,
                        UpdateOutcome::ReceiverClosed => return Ok(()),
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

    /// Delegates to [`Self::request_approval_attributed`] and drops the
    /// provenance, so the prompt/timeout logic lives in exactly one place.
    async fn request_approval(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed(recipient, request)
            .await?
            .map(|attributed| attributed.response))
    }

    async fn request_approval_attributed(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        // Parse recipient for chat_id + optional thread_id ("chat_id:thread_id" format).
        let (chat_id, thread_id) = recipient
            .split_once(':')
            .map_or((recipient, None), |(c, t)| (c, Some(t)));

        // Unique key embedded in callback_data so listen() can route the tap.
        let approval_id = uuid::Uuid::new_v4().to_string();

        let heading = i18n::get_required_cli_string("channel-approval-heading");
        let tool_label = i18n::get_required_cli_string("channel-approval-tool-label");
        let tap_instruction = i18n::get_required_cli_string("channel-approval-tap-instruction");
        let btn_approve = i18n::get_required_cli_string("channel-approval-btn-approve");
        let btn_deny = i18n::get_required_cli_string("channel-approval-btn-deny");
        let btn_always = i18n::get_required_cli_string("channel-approval-btn-always");

        let tool = Self::escape_html(&request.tool_name);
        let args = Self::escape_html(&request.arguments_summary);
        let text = format!(
            "\u{1f527} <b>{heading}</b>\n\n\
             {tool_label}: <code>{tool}</code>\n\
             {args}\n\n\
             {tap_instruction}",
        );

        let reply_markup = serde_json::json!({
            "inline_keyboard": [[
                { "text": format!("✅ {btn_approve}"),  "callback_data": format!("approval:{}:approve", approval_id) },
                { "text": format!("❌ {btn_deny}"),     "callback_data": format!("approval:{}:deny", approval_id) },
                { "text": format!("✅✅ {btn_always}"), "callback_data": format!("approval:{}:always", approval_id) },
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
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        self.pending_approvals.lock().await.insert(
            approval_id.clone(),
            PendingApproval {
                sender: tx,
                tool_name: request.tool_name.clone(),
            },
        );

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
                    "🔧 {heading}\n\n{tool_label}: {}\n{}\n\n{tap_instruction}",
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
        // `channels.telegram.approval_timeout_secs` (default 120s). The
        // pending entry is the single resolution claim: exactly one side
        // removes it, and that side decides. When the deadline fires the
        // waiter claims the entry and denies; if a callback already claimed
        // it, the operator's response is in flight on the channel and is
        // consumed instead of overridden, so the published card can never
        // disagree with the runtime's recorded outcome.
        let result =
            match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), &mut rx)
                .await
            {
                Ok(Ok(response)) => Some(
                    zeroclaw_api::channel::AttributedApprovalResponse::operator(response),
                ),
                Ok(Err(_)) => {
                    // Sender dropped — clean up and deny. Nobody tapped.
                    self.pending_approvals.lock().await.remove(&approval_id);
                    Some(
                        zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                            ChannelApprovalResponse::Deny,
                            zeroclaw_api::channel::ApprovalSource::Unreachable,
                        ),
                    )
                }
                Err(_) => Some(self.resolve_after_deadline(&approval_id, &mut rx).await),
            };

        Ok(result)
    }
}

#[cfg(test)]
impl UpdateDisposition {
    /// Unwrap a `Parsed` disposition in tests, panicking with `context` on the
    /// skip/retry variants. Keeps parser regressions terse now that the parser
    /// returns a disposition rather than an `Option`. Defined alongside the
    /// test module rather than beside the enum so the first `#[cfg(test)]` in
    /// this file stays after the production code, keeping the config-isolation
    /// architecture gate's test-region scan off `persist_allowed_identity`.
    pub(crate) fn expect_parsed(self, context: &str) -> ChannelMessage {
        match self {
            UpdateDisposition::Parsed(msg) => *msg,
            UpdateDisposition::SkipPermanent => panic!("{context}: got SkipPermanent"),
            UpdateDisposition::RetryTransient => panic!("{context}: got RetryTransient"),
        }
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

    #[tokio::test]
    async fn update_draft_lifecycle_only_edits_partial_streaming_drafts() {
        use wiremock::matchers::{body_json, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let running_tool_text =
            crate::util::localized_lifecycle_progress(ProgressEvent::RunningTool);
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/editMessageText$"))
            .and(body_json(serde_json::json!({
                "chat_id": "123",
                "message_id": 42,
                "text": running_tool_text,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 42 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        for stream_mode in [StreamMode::Off, StreamMode::MultiMessage] {
            let channel = TelegramChannel::new(
                "fake-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["*".into()]),
                false,
            )
            .with_streaming(stream_mode, 0)
            .with_api_base(mock_server.uri());

            channel
                .update_draft_lifecycle("123", "42", ProgressEvent::RunningTool)
                .await
                .unwrap();
        }

        let throttled = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_streaming(StreamMode::Partial, 60_000)
        .with_api_base(mock_server.uri());
        throttled
            .last_draft_edit
            .lock()
            .insert("123".to_string(), std::time::Instant::now());
        throttled
            .update_draft_lifecycle("123", "42", ProgressEvent::RunningTool)
            .await
            .unwrap();

        let partial = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_streaming(StreamMode::Partial, 0)
        .with_api_base(mock_server.uri());

        partial
            .update_draft_lifecycle("123", "42", ProgressEvent::RunningTool)
            .await
            .unwrap();
    }

    /// Raw tool status carries the tool name plus a command, path, or query.
    /// Only the typed lifecycle event may reach Telegram.
    #[tokio::test]
    async fn raw_tool_status_never_reaches_telegram() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const RAW_TOOL_STATUS: &str =
            "\u{23f3} shell: cat /home/example/.ssh/id_rsa && export API_KEY=placeholder-secret\n";

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/editMessageText$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 42 }
            })))
            .mount(&mock_server)
            .await;

        let partial = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_streaming(StreamMode::Partial, 0)
        .with_api_base(mock_server.uri());

        partial
            .update_draft_progress("123", "42", RAW_TOOL_STATUS)
            .await
            .unwrap();
        partial
            .update_draft_lifecycle("123", "42", ProgressEvent::RunningTool)
            .await
            .unwrap();

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            1,
            "only the typed lifecycle event should reach Telegram"
        );
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body["text"],
            crate::util::localized_lifecycle_progress(ProgressEvent::RunningTool)
        );
        let raw = String::from_utf8_lossy(&requests[0].body);
        for leaked in [
            "shell",
            "cat ",
            ".ssh",
            "id_rsa",
            "API_KEY",
            "placeholder-secret",
        ] {
            assert!(
                !raw.contains(leaked),
                "tool status detail '{leaked}' leaked to Telegram"
            );
        }
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
                "message_thread_id": 789,
                "is_topic_message": true
            }
        });

        let msg = ch
            .parse_update_message(&update)
            .expect("message with thread_id should parse");

        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.reply_target, "-100200300:789");
        assert_eq!(msg.thread_ts.as_deref(), Some("789"));
        assert_eq!(msg.content, "hello from topic");
        assert_eq!(msg.id, "telegram_-100200300_42");
    }

    #[test]
    fn parse_update_reply_thread_shares_main_chat_history_key() {
        // Telegram sets `message_thread_id` for ordinary reply-threads in
        // supergroups too, but WITHOUT `is_topic_message`. Those are not topic
        // boundaries: they must continue the main chat conversation, so
        // `thread_ts` stays None and `reply_target` carries no `:tid` suffix —
        // giving the same conversation-history key as a plain message in the chat.
        let ch = TelegramChannel::new(
            "token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        );
        let reply_thread = serde_json::json!({
            "update_id": 4,
            "message": {
                "message_id": 43,
                "text": "and another thing",
                "from": { "id": 555, "username": "alice" },
                "chat": { "id": -100_200_300 },
                "message_thread_id": 789,
                "reply_to_message": { "message_id": 40, "from": { "username": "bot" }, "text": "earlier" }
            }
        });
        let msg = ch
            .parse_update_message(&reply_thread)
            .expect("reply-thread message should parse");
        assert_eq!(
            msg.reply_target, "-100200300",
            "a reply-thread (no is_topic_message) must resolve to the main chat, not a :tid topic"
        );
        assert_eq!(
            msg.thread_ts, None,
            "a reply-thread must not set thread_ts, or it forks the conversation history key"
        );

        // Same chat + same sender, plain message: identical reply_target/thread_ts,
        // so both land in one history bucket.
        let plain = serde_json::json!({
            "update_id": 5,
            "message": {
                "message_id": 44,
                "text": "plain message",
                "from": { "id": 555, "username": "alice" },
                "chat": { "id": -100_200_300 }
            }
        });
        let plain_msg = ch
            .parse_update_message(&plain)
            .expect("plain message should parse");
        assert_eq!(plain_msg.reply_target, msg.reply_target);
        assert_eq!(plain_msg.thread_ts, msg.thread_ts);
    }

    /// A real, decodable 1x1 baseline JPEG (grayscale, optimized Huffman
    /// tables). Used instead of a header-only byte stub so the attachment
    /// fixture stays valid if image validation ever tightens.
    fn tiny_jpeg() -> Vec<u8> {
        vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x20, 0x16, 0x18,
            0x1C, 0x18, 0x14, 0x20, 0x1C, 0x1A, 0x1C, 0x24, 0x22, 0x20, 0x26, 0x30, 0x50, 0x34,
            0x30, 0x2C, 0x2C, 0x30, 0x62, 0x46, 0x4A, 0x3A, 0x50, 0x74, 0x66, 0x7A, 0x78, 0x72,
            0x66, 0x70, 0x6E, 0x80, 0x90, 0xB8, 0x9C, 0x80, 0x88, 0xAE, 0x8A, 0x6E, 0x70, 0xA0,
            0xDA, 0xA2, 0xAE, 0xBE, 0xC4, 0xCE, 0xD0, 0xCE, 0x7C, 0x9A, 0xE2, 0xF2, 0xE0, 0xC8,
            0xF0, 0xB8, 0xCA, 0xCE, 0xC6, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01,
            0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xC4,
            0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00,
            0x3F, 0xFF, 0xD9,
        ]
    }

    #[tokio::test]
    async fn attachment_parser_applies_the_topic_thread_gate() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The attachment parser (`try_parse_attachment_message`) is one of the
        // three parse paths wired to `topic_thread_id`. A genuine forum topic
        // must keep `chat_id:thread_id` + `thread_ts`, while an ordinary
        // reply-thread (no `is_topic_message`) must resolve to the main chat so
        // it lands in the same conversation-history bucket as a plain message.
        let workspace = tempfile::tempdir().unwrap();
        let mock_server = MockServer::start().await;
        let photo_bytes = tiny_jpeg();
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "file_path": "photos/file_1.jpg" }
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/file/bot[^/]+/photos/file_1\.jpg$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(photo_bytes.clone()))
            .mount(&mock_server)
            .await;

        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_api_base(mock_server.uri())
        .with_workspace_dir(workspace.path().to_path_buf());

        // Genuine forum topic: isolated.
        let topic = ch
            .try_parse_attachment_message(&serde_json::json!({
                "message": {
                    "message_id": 42,
                    "chat": { "id": -100_200_300, "type": "supergroup" },
                    "from": { "username": "alice", "id": 99 },
                    "photo": [ { "file_id": "best", "file_size": 20 } ],
                    "caption": "look at this",
                    "message_thread_id": 789,
                    "is_topic_message": true
                }
            }))
            .await
            .expect_parsed("topic photo should parse");
        assert_eq!(topic.reply_target, "-100200300:789");
        assert_eq!(topic.thread_ts.as_deref(), Some("789"));

        // Ordinary reply-thread (no is_topic_message): continues the main chat.
        let reply = ch
            .try_parse_attachment_message(&serde_json::json!({
                "message": {
                    "message_id": 43,
                    "chat": { "id": -100_200_300, "type": "supergroup" },
                    "from": { "username": "alice", "id": 99 },
                    "photo": [ { "file_id": "best", "file_size": 20 } ],
                    "caption": "and this",
                    "message_thread_id": 789,
                    "reply_to_message": { "message_id": 40, "from": { "username": "bob" }, "text": "x" }
                }
            }))
            .await
            .expect_parsed("reply-thread photo should parse");
        assert_eq!(reply.reply_target, "-100200300");
        assert_eq!(reply.thread_ts, None);
    }

    #[tokio::test]
    async fn voice_parser_applies_the_topic_thread_gate() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The voice parser (`try_parse_voice_message`) is the third parse path
        // wired to `topic_thread_id`. Same contract as the attachment path: a
        // genuine topic isolates, an ordinary reply-thread continues the main
        // chat. Reaching the parsed message requires the full transcription
        // path, so mock getFile, the file download, and the Whisper endpoint.
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "file_path": "voice/file_1.ogg" }
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/file/bot[^/]+/voice/file_1\.ogg$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 64]))
            .mount(&mock_server)
            .await;
        // Groq posts the audio to `config.api_url`.
        Mock::given(method("POST"))
            .and(path_regex(r"/v1/audio/transcriptions$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "text": "hello there" })),
            )
            .mount(&mock_server)
            .await;

        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            api_url: format!("{}/v1/audio/transcriptions", mock_server.uri()),
            max_duration_secs: 120,
            ..Default::default()
        };
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_api_base(mock_server.uri())
        .with_transcription(tc);

        // Genuine forum topic: isolated.
        let topic = ch
            .try_parse_voice_message(&serde_json::json!({
                "message": {
                    "message_id": 51,
                    "voice": { "file_id": "voice_file", "duration": 4 },
                    "from": { "id": 555, "username": "alice" },
                    "chat": { "id": -100_200_300, "type": "supergroup" },
                    "message_thread_id": 789,
                    "is_topic_message": true
                }
            }))
            .await
            .expect_parsed("topic voice should parse");
        assert_eq!(topic.reply_target, "-100200300:789");
        assert_eq!(topic.thread_ts.as_deref(), Some("789"));

        // Ordinary reply-thread (no is_topic_message): continues the main chat.
        let reply = ch
            .try_parse_voice_message(&serde_json::json!({
                "message": {
                    "message_id": 52,
                    "voice": { "file_id": "voice_file", "duration": 4 },
                    "from": { "id": 555, "username": "alice" },
                    "chat": { "id": -100_200_300, "type": "supergroup" },
                    "message_thread_id": 789,
                    "reply_to_message": { "message_id": 40, "from": { "username": "bob" }, "text": "x" }
                }
            }))
            .await
            .expect_parsed("reply-thread voice should parse");
        assert_eq!(reply.reply_target, "-100200300");
        assert_eq!(reply.thread_ts, None);
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
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Keep this unit test hermetic: a fake token against the official API
        // can wait indefinitely when CI networking degrades.
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/sendPhoto$"))
            .respond_with(ResponseTemplate::new(200))
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
        // Minimal valid PNG header bytes
        let file_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

        ch.send_photo_bytes("123456", None, file_bytes.clone(), "test.png", None)
            .await
            .expect("mock Telegram API should accept photo upload");

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body = &requests[0].body;
        let form = String::from_utf8_lossy(body);
        assert!(form.contains("name=\"chat_id\""));
        assert!(form.contains("123456"));
        assert!(form.contains("name=\"photo\""));
        assert!(form.contains("filename=\"test.png\""));
        assert!(
            body.windows(file_bytes.len())
                .any(|window| window == file_bytes),
            "multipart body must contain the original photo bytes"
        );
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
        assert!(matches!(parsed, UpdateDisposition::SkipPermanent));
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
        assert!(matches!(parsed, UpdateDisposition::SkipPermanent));
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
        assert!(matches!(parsed, UpdateDisposition::SkipPermanent));
        assert!(ch.voice_transcriptions.lock().is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // listen(): inbound offset must only advance past updates that were
    // actually enqueued (or permanently skipped) — never past an update
    // whose parsing failed transiently or whose delivery never completed.
    // ─────────────────────────────────────────────────────────────────────

    fn telegram_text_update(
        update_id: i64,
        message_id: i64,
        chat_id: i64,
        username: &str,
        text: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "update_id": update_id,
            "message": {
                "message_id": message_id,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": message_id + 100_000, "username": username},
                "text": text,
            }
        })
    }

    fn telegram_document_update(
        update_id: i64,
        message_id: i64,
        chat_id: i64,
        username: &str,
        file_id: &str,
        file_name: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "update_id": update_id,
            "message": {
                "message_id": message_id,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": message_id + 100_000, "username": username},
                "document": {"file_id": file_id, "file_name": file_name},
            }
        })
    }

    fn telegram_voice_update(
        update_id: i64,
        message_id: i64,
        chat_id: i64,
        username: &str,
        file_id: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "update_id": update_id,
            "message": {
                "message_id": message_id,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": message_id + 100_000, "username": username},
                "voice": {"file_id": file_id, "duration": 3},
            }
        })
    }

    /// Mount the startup probe (`getUpdates` with `"timeout": 0`) that
    /// `listen()` issues once before entering the main long-poll loop.
    /// Responds with an empty backlog so the probe succeeds immediately.
    async fn mount_telegram_startup_probe(mock_server: &wiremock::MockServer) {
        use wiremock::matchers::{body_partial_json, method, path_regex};
        use wiremock::{Mock, ResponseTemplate};

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/getUpdates$"))
            .and(body_partial_json(serde_json::json!({"timeout": 0})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": []})),
            )
            .mount(mock_server)
            .await;
    }

    /// Mount the startup probe (`getUpdates` with `"timeout": 0`) so its
    /// single response carries `update` in `result`, simulating a message
    /// that queued up on Telegram's side while the listener was down (e.g.
    /// across a restart) and is waiting at the current offset.
    async fn mount_telegram_startup_probe_with_queued_update(
        mock_server: &wiremock::MockServer,
        update: serde_json::Value,
    ) {
        use wiremock::matchers::{body_partial_json, method, path_regex};
        use wiremock::{Mock, ResponseTemplate};

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/getUpdates$"))
            .and(body_partial_json(serde_json::json!({"timeout": 0})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": [update]})),
            )
            .mount(mock_server)
            .await;
    }

    /// Mount a main-loop `getUpdates` responder (`"timeout": 30`) matched on
    /// the exact `offset` the request carries, replying `ok` with `result`.
    async fn mount_telegram_get_updates(
        mock_server: &wiremock::MockServer,
        offset: i64,
        result: serde_json::Value,
    ) {
        use wiremock::matchers::{body_partial_json, method, path_regex};
        use wiremock::{Mock, ResponseTemplate};

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/getUpdates$"))
            .and(body_partial_json(
                serde_json::json!({"offset": offset, "timeout": 30}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": result
            })))
            .mount(mock_server)
            .await;
    }

    /// Mount a `sendMessage` responder that must be hit exactly
    /// `expect_calls` times. When `body_fragments` is non-empty the mock
    /// only matches requests whose body contains every fragment, so a
    /// send with the wrong chat or the wrong message goes unmatched and
    /// fails the expectation instead of passing vacuously.
    ///
    /// That narrowing has a gap of its own: a *wrong* `sendMessage` (bad
    /// chat, bad text) that misses every fragment mock still goes
    /// unmatched by the mock above, gets wiremock's default 404, and is
    /// silently swallowed by the production `let _ = self.send(...)` —
    /// so the test would still pass even though an extra, unexpected
    /// notice went out. When `body_fragments` is non-empty, also mount a
    /// catch-all matching any `sendMessage` that does NOT contain every
    /// expected fragment, with a zero-call expectation, so that stray
    /// request fails the test instead of disappearing into a 404. (When
    /// `body_fragments` is empty the primary mock above already matches —
    /// and bounds — every `sendMessage`, so no catch-all is needed.)
    async fn mount_telegram_send_message_ok(
        mock_server: &wiremock::MockServer,
        expect_calls: u64,
        body_fragments: &[&str],
    ) {
        use wiremock::matchers::{body_string_contains, method, path_regex};
        use wiremock::{Mock, Request, ResponseTemplate};

        let mut mock = Mock::given(method("POST")).and(path_regex(r"/bot[^/]+/sendMessage$"));
        for fragment in body_fragments {
            mock = mock.and(body_string_contains(*fragment));
        }
        mock.respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true, "result": {}})),
        )
        .expect(expect_calls)
        .mount(mock_server)
        .await;

        if !body_fragments.is_empty() {
            let expected_fragments: Vec<String> =
                body_fragments.iter().map(|f| f.to_string()).collect();
            Mock::given(method("POST"))
                .and(path_regex(r"/bot[^/]+/sendMessage$"))
                .and(move |request: &Request| {
                    let body = std::str::from_utf8(&request.body).unwrap_or_default();
                    !expected_fragments.iter().all(|f| body.contains(f.as_str()))
                })
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"ok": true, "result": {}})),
                )
                .expect(0)
                .mount(mock_server)
                .await;
        }
    }

    /// Every main-loop `getUpdates` request body (`"timeout": 30`, excluding
    /// the startup probe), in the order the mock server received them.
    async fn telegram_main_loop_getupdates_bodies(
        mock_server: &wiremock::MockServer,
    ) -> Vec<serde_json::Value> {
        mock_server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.url.path().ends_with("/getUpdates"))
            .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
            .filter(|b| b.get("timeout").and_then(serde_json::Value::as_i64) == Some(30))
            .collect()
    }

    /// Upper bound for the "this must not hang" waits in the `listen` tests.
    ///
    /// These guards exist to fail a genuine hang, not to assert how quickly
    /// the long-poll loop runs. `scripts/ci/parallel_runtime_test_gate.sh`
    /// runs the suite at 16 threads, and under that contention the previous
    /// 5s and 10s budgets stopped being hang guards and became scheduling
    /// assertions. Delivery here is sub-second when it is not hung, and the
    /// slowest path waits through a real retry sequence that completes well
    /// inside this bound, so ordinary runner load cannot reach it.
    const LISTEN_HANG_GUARD: Duration = Duration::from_secs(30);

    /// Wait until a main-loop `getUpdates` call carrying `offset` shows up.
    ///
    /// Panics with the offsets actually observed. This replaced a `bool`
    /// return that every caller asserted as "the offset never advanced",
    /// which is a claim a deadline cannot support: on a loaded runner the
    /// same `false` means "not yet". `context` names what the offset was
    /// supposed to move past, so the panic says which step is in question
    /// without pretending to know why.
    async fn telegram_expect_main_loop_offset(
        mock_server: &wiremock::MockServer,
        offset: i64,
        guard: Duration,
        context: &str,
    ) {
        let deadline = tokio::time::Instant::now() + guard;
        loop {
            let seen: Vec<i64> = telegram_main_loop_getupdates_bodies(mock_server)
                .await
                .iter()
                .filter_map(|b| b.get("offset").and_then(serde_json::Value::as_i64))
                .collect();
            if seen.contains(&offset) {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "main loop did not request offset {offset} ({context}) within \
                     {guard:?}; observed offsets {seen:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// A transient failure downloading an attachment (`getFile` 500) must
    /// leave the offset un-advanced so the next poll re-fetches the same
    /// update; once the download succeeds, the offset advances past it.
    #[tokio::test]
    async fn listen_retries_transient_download_failure_at_same_offset_then_advances() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid = 1_000;
        let update = telegram_document_update(uid, 5, 555, "alice", "file123", "report.pdf");

        mount_telegram_get_updates(&mock_server, 0, serde_json::json!([update])).await;

        // First getFile attempt fails transiently; the retry succeeds.
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"file_path": "documents/report.pdf"}
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/file/bot[^/]+/.*$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pdf bytes".to_vec()))
            .mount(&mock_server)
            .await;

        // Keep the loop fed once the offset advances past the update.
        mount_telegram_get_updates(&mock_server, uid + 1, serde_json::json!([])).await;

        let workspace = tempfile::tempdir().unwrap();
        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri())
            .with_workspace_dir(workspace.path().to_path_buf()),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let msg = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for the attachment message")
            .expect("channel closed before delivering the attachment message");
        assert!(
            msg.content.contains("report.pdf"),
            "unexpected content: {}",
            msg.content
        );

        let main_loop_bodies = telegram_main_loop_getupdates_bodies(&mock_server).await;
        assert!(
            main_loop_bodies.len() >= 2,
            "expected at least 2 main-loop getUpdates requests (initial attempt + retry), got {}",
            main_loop_bodies.len()
        );
        for body in &main_loop_bodies[..2] {
            assert_eq!(
                body.get("offset").and_then(serde_json::Value::as_i64),
                Some(0),
                "offset must not advance while the download keeps failing transiently"
            );
        }

        telegram_expect_main_loop_offset(
            &mock_server,
            uid + 1,
            LISTEN_HANG_GUARD,
            "past the update whose retry succeeded",
        )
        .await;

        handle.abort();
    }

    /// If the channel receiver is dropped mid-batch (after the first update
    /// is delivered but before the second's `tx.send` completes), `listen()`
    /// must return `Ok(())` without ever having polled again — so it can
    /// never have acknowledged (via a subsequent `getUpdates` offset) the
    /// second, undelivered update.
    #[tokio::test]
    async fn listen_mid_batch_receiver_drop_never_advances_past_delivered() {
        use wiremock::MockServer;

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid1 = 3_000;
        let uid2 = 3_001;
        let update1 = telegram_text_update(uid1, 10, 777, "alice", "hello");
        let update2 = telegram_text_update(uid2, 11, 777, "alice", "world");

        mount_telegram_get_updates(&mock_server, 0, serde_json::json!([update1, update2])).await;

        let ch = TelegramChannel::new(
            "test-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["alice".to_string()]),
            false,
        )
        .with_api_base(mock_server.uri());

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let handle = zeroclaw_spawn::spawn!(async move { ch.listen(tx).await });

        let first = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for first message")
            .expect("channel closed before first message");
        assert_eq!(first.content, "hello");

        // Drop synchronously (no intervening await) so the second update's
        // `tx.send` observes a closed channel.
        drop(rx);

        let result = tokio::time::timeout(LISTEN_HANG_GUARD, handle)
            .await
            .expect("listen() task timed out")
            .expect("listen() task panicked");
        assert!(result.is_ok());

        let main_loop_bodies = telegram_main_loop_getupdates_bodies(&mock_server).await;
        assert_eq!(
            main_loop_bodies.len(),
            1,
            "listen() must return immediately after the failed send, issuing no further poll"
        );
        assert_eq!(
            main_loop_bodies[0]
                .get("offset")
                .and_then(serde_json::Value::as_i64),
            Some(0),
            "the only getUpdates request must never carry an offset past the undelivered second update"
        );
    }

    /// An unauthorized-sender update is a permanent skip: it must still
    /// advance the offset (so it's not retried forever), while an
    /// authorized update right after it is delivered normally.
    #[tokio::test]
    async fn listen_permanent_skip_advances_past_unauthorized_update() {
        use wiremock::MockServer;

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid1 = 4_000; // unauthorized sender
        let uid2 = 4_001; // authorized sender
        let unauthorized_update =
            telegram_text_update(uid1, 20, 888, "mallory", "give me the keys");
        let authorized_update = telegram_text_update(uid2, 21, 888, "alice", "world");

        mount_telegram_get_updates(
            &mock_server,
            0,
            serde_json::json!([unauthorized_update, authorized_update]),
        )
        .await;

        // Keep the loop fed once both updates are acknowledged, so we can
        // observe the advanced offset without a stray unmatched request.
        mount_telegram_get_updates(&mock_server, uid2 + 1, serde_json::json!([])).await;

        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri()),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let msg = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for the authorized message")
            .expect("channel closed before delivering the authorized message");
        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.content, "world");

        // The unauthorized update must never reach `tx` — no retry loop,
        // no eventual delivery.
        let extra = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            extra.is_err(),
            "unexpected extra message delivered: {extra:?}"
        );

        telegram_expect_main_loop_offset(
            &mock_server,
            uid2 + 1,
            LISTEN_HANG_GUARD,
            "past the unauthorized update to the next expected value",
        )
        .await;

        handle.abort();
    }

    /// A transient failure that outlasts the former three-attempt budget must
    /// remain unacknowledged. Later updates in the same ordered batch cannot
    /// pass it; once the failing update recovers, all messages are delivered
    /// in order and the offset advances past the whole batch.
    #[tokio::test]
    async fn listen_ordered_batch_recovers_after_extended_transient_failure() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid1 = 2_000;
        let uid2 = 2_001;
        let uid3 = 2_002;
        let first = telegram_text_update(uid1, 6, 666, "alice", "first");
        let failing = telegram_document_update(uid2, 7, 666, "alice", "file456", "report.pdf");
        let later = telegram_text_update(uid3, 8, 666, "alice", "third");

        mount_telegram_get_updates(
            &mock_server,
            0,
            serde_json::json!([first, failing.clone(), later.clone()]),
        )
        .await;
        mount_telegram_get_updates(&mock_server, uid1 + 1, serde_json::json!([failing, later]))
            .await;
        mount_telegram_get_updates(&mock_server, uid3 + 1, serde_json::json!([])).await;

        // Four failures outlast the former three-attempt budget. The fifth
        // attempt succeeds, proving elapsed retries do not reclassify the
        // update as a permanent skip.
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(4)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"file_path": "documents/report.pdf"}
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/file/bot[^/]+/.*$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pdf bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let workspace = tempfile::tempdir().unwrap();
        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri())
            .with_workspace_dir(workspace.path().to_path_buf()),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let first_message = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for the first message")
            .expect("channel closed before delivering the first message");
        assert_eq!(first_message.content, "first");

        let recovered = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for the recovered attachment")
            .expect("channel closed before delivering the recovered attachment");
        assert!(
            recovered.content.contains("report.pdf"),
            "the failed update must recover before the later update, got: {}",
            recovered.content
        );
        let third_message = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for the later message")
            .expect("channel closed before delivering the later message");
        assert_eq!(third_message.content, "third");

        let main_loop_bodies = telegram_main_loop_getupdates_bodies(&mock_server).await;
        let retry_polls = main_loop_bodies
            .iter()
            .filter(|body| body.get("offset").and_then(serde_json::Value::as_i64) == Some(uid1 + 1))
            .count();
        assert!(
            retry_polls >= 4,
            "expected retries beyond the former three-attempt budget at the blocked offset, got {retry_polls}"
        );
        telegram_expect_main_loop_offset(
            &mock_server,
            uid3 + 1,
            LISTEN_HANG_GUARD,
            "past the ordered batch after recovery",
        )
        .await;

        handle.abort();
    }

    /// Drive `listen()` with `unauthorized_update` (sent by "zeroclaw_unauthorized",
    /// who is not on the allowlist) followed by an authorized text update
    /// in the same chat, asserting the shared unauthorized-update
    /// contract: no `getFile` download, exactly `expected_notices`
    /// unauthorized-approval notices sent to the update's chat, the
    /// offset advancing past both updates, and only the authorized
    /// message reaching `tx`. `decorate` lets each caller add
    /// channel-specific config (transcription, workspace dir, ...).
    async fn assert_listen_skips_unauthorized_update(
        unauthorized_update: serde_json::Value,
        decorate: impl FnOnce(TelegramChannel) -> TelegramChannel,
        expected_notices: u64,
    ) {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid1 = unauthorized_update
            .get("update_id")
            .and_then(serde_json::Value::as_i64)
            .expect("unauthorized update must carry an update_id");
        let uid2 = uid1 + 1; // authorized text sender right behind it
        let chat_id = unauthorized_update["message"]["chat"]["id"]
            .as_i64()
            .expect("unauthorized update must carry a chat id");
        let text_update = telegram_text_update(uid2, 1, chat_id, "zeroclaw_user", "world");

        mount_telegram_get_updates(
            &mock_server,
            0,
            serde_json::json!([unauthorized_update, text_update]),
        )
        .await;
        mount_telegram_get_updates(&mock_server, uid2 + 1, serde_json::json!([])).await;

        // The unauthorized update must be rejected before any file I/O —
        // this mock existing with `.expect(0)` is the assertion.
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"file_path": "unauthorized/never-downloaded"}
            })))
            .expect(0)
            .mount(&mock_server)
            .await;

        // With zero expected notices any sendMessage at all must fail the
        // expectation; otherwise pin the notice to this chat and to the
        // approval text so a stray send cannot satisfy the mock.
        let chat_fragment = format!(r#""chat_id":"{chat_id}""#);
        let fragments = if expected_notices == 0 {
            vec![]
        } else {
            vec![chat_fragment.as_str(), "requires operator approval"]
        };
        mount_telegram_send_message_ok(&mock_server, expected_notices, &fragments).await;

        let ch = Arc::new(decorate(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["zeroclaw_user".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri()),
        ));

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let msg = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for the authorized message")
            .expect("channel closed before delivering the authorized message");
        assert_eq!(msg.sender, "zeroclaw_user");
        assert_eq!(msg.content, "world");

        // The unauthorized update must never be delivered.
        let extra = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            extra.is_err(),
            "unexpected extra message delivered: {extra:?}"
        );

        telegram_expect_main_loop_offset(
            &mock_server,
            uid2 + 1,
            LISTEN_HANG_GUARD,
            "past the unauthorized update",
        )
        .await;

        handle.abort();
    }

    /// A restart's startup probe (`getUpdates` with `"timeout": 0`) can come
    /// back with updates that queued up on Telegram's side while the
    /// listener was down. Those updates must go through the same
    /// delivered/permanent-skip/retry-transient disposition path as the
    /// main loop: if delivery of a queued update fails transiently, the
    /// offset must NOT advance past it in the probe, and the update must
    /// survive, unadvanced, until a later poll (here, the main loop's very
    /// next request at the same offset) can actually deliver it.
    #[tokio::test]
    async fn listen_restart_probe_queued_update_survives_transient_failure_until_delivered() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        let uid = 6_000;
        let update = telegram_document_update(uid, 40, 111, "alice", "file999", "queued.pdf");

        // The startup probe's one and only response carries the update that
        // was queued while the listener was offline, simulating a restart.
        mount_telegram_startup_probe_with_queued_update(&mock_server, update.clone()).await;

        // The offset must stay at 0 across the probe's transient failure, so
        // the main loop re-polls at the same offset and sees the same
        // still-queued update again.
        mount_telegram_get_updates(&mock_server, 0, serde_json::json!([update])).await;

        // First getFile attempt (from the probe) fails transiently; the
        // retry (from the main loop's first poll) succeeds.
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"file_path": "documents/queued.pdf"}
            })))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/file/bot[^/]+/.*$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pdf bytes".to_vec()))
            .mount(&mock_server)
            .await;

        // Keep the loop fed once the offset advances past the update.
        mount_telegram_get_updates(&mock_server, uid + 1, serde_json::json!([])).await;

        let workspace = tempfile::tempdir().unwrap();
        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri())
            .with_workspace_dir(workspace.path().to_path_buf()),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        let msg = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out waiting for the queued attachment message")
            .expect("channel closed before delivering the queued attachment message");
        assert!(
            msg.content.contains("queued.pdf"),
            "unexpected content: {}",
            msg.content
        );

        // Exactly one main-loop poll (timeout: 30) must have happened at the
        // still-unadvanced offset 0 before the offset moved past the update:
        // the probe's own transient failure must not have advanced it.
        let old_offset_polls = telegram_main_loop_getupdates_bodies(&mock_server)
            .await
            .iter()
            .filter(|b| b.get("offset").and_then(serde_json::Value::as_i64) == Some(0))
            .count();
        assert_eq!(
            old_offset_polls, 1,
            "offset must have stayed at 0 (unadvanced by the probe) for exactly one main-loop retry"
        );

        telegram_expect_main_loop_offset(
            &mock_server,
            uid + 1,
            LISTEN_HANG_GUARD,
            "past the queued update whose retry succeeded",
        )
        .await;

        // The queued update must be delivered exactly once, never twice.
        let extra = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            extra.is_err(),
            "unexpected extra message delivered: {extra:?}"
        );

        handle.abort();
    }

    /// Build a `callback_query` update carrying an inline-keyboard approval
    /// tap, as Telegram delivers it to `getUpdates`.
    fn telegram_callback_update(
        update_id: i64,
        callback_id: &str,
        approval_id: &str,
        action: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "update_id": update_id,
            "callback_query": {
                "id": callback_id,
                "from": {"id": 900_001, "username": "alice"},
                "data": format!("approval:{approval_id}:{action}"),
            }
        })
    }

    /// Approval acknowledgements were previously localized through the
    /// runtime Fluent catalogue. This PR relocates the whole `callback_query`
    /// arm into `process_update`, so the move must not silently re-introduce
    /// hard-coded English ack text.
    ///
    /// This drives a real `callback_query` through the listener and asserts
    /// the posted `answerCallbackQuery` body's `text` is rebuilt from the
    /// SAME `channel-telegram-approval-ack-*` keys the implementation uses —
    /// locale-agnostic, so it holds whatever locale the test process
    /// resolves to, and fails if any arm is replaced by a literal.
    #[tokio::test]
    async fn listen_callback_approval_ack_uses_fluent_catalogue() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        // One update per action, so every catalogue-backed arm is exercised
        // in a single listener run: approve, always, deny, and the unknown
        // fallback.
        let actions = ["approve", "always", "deny", "bogus"];
        let updates: Vec<serde_json::Value> = actions
            .iter()
            .enumerate()
            .map(|(i, action)| {
                telegram_callback_update(
                    7_000 + i as i64,
                    &format!("cb{i}"),
                    "11111111-2222-3333-4444-555555555555",
                    action,
                )
            })
            .collect();
        let last_uid = 7_000 + actions.len() as i64 - 1;

        mount_telegram_get_updates(&mock_server, 0, serde_json::json!(updates)).await;
        mount_telegram_get_updates(&mock_server, last_uid + 1, serde_json::json!([])).await;

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/answerCallbackQuery$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": true})),
            )
            .mount(&mock_server)
            .await;

        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri()),
        );

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // A callback is terminal for inbound processing, so the offset must
        // advance past the whole batch; waiting on that also guarantees every
        // answerCallbackQuery has been posted before we inspect them.
        telegram_expect_main_loop_offset(
            &mock_server,
            last_uid + 1,
            LISTEN_HANG_GUARD,
            "past the callback batch",
        )
        .await;

        let ack_texts: Vec<String> = mock_server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.url.path().ends_with("/answerCallbackQuery"))
            .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
            .filter_map(|b| {
                b.get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
            .collect();

        // Rebuild the expectation through the catalogue, not from literals:
        // a wiring regression that stops calling i18n, or a typo'd key,
        // changes this and fails the assertion. These callbacks carry no
        // pending entry, so every valid action gets the already-resolved
        // toast and only the unknown action gets its own arm; the won-claim
        // action acks are pinned by callback_wins_claim and the scanner.
        let stale = i18n::get_required_cli_string("channel-telegram-approval-ack-already-resolved");
        let expected = vec![
            format!("⏳ {stale}"),
            format!("⏳ {stale}"),
            format!("⏳ {stale}"),
            format!(
                "⚠️ {}",
                i18n::get_required_cli_string("channel-telegram-approval-ack-unknown")
            ),
        ];
        assert_eq!(
            ack_texts, expected,
            "answerCallbackQuery text must come from the Fluent catalogue, not hard-coded English"
        );

        handle.abort();
    }

    /// The source region of `process_update`'s `callback_query` arm that
    /// builds the acknowledgement text, delimited by the `answer_text`
    /// binding and the `answer_body` that consumes it.
    ///
    /// Read from the compiled-in source so the assertion tracks the file
    /// rather than a copy that can drift.
    fn callback_ack_source_region() -> &'static str {
        const SRC: &str = include_str!("telegram.rs");
        let start = SRC
            .find("let answer_text = if resolved_tool.is_some() {")
            .expect("callback ack arm: `let answer_text = if resolved_tool.is_some() {` not found");
        let rest = &SRC[start..];
        let end = rest
            .find("let answer_body")
            .expect("callback ack arm: `let answer_body` terminator not found");
        &rest[..end]
    }

    /// Companion to `listen_callback_approval_ack_uses_fluent_catalogue`.
    ///
    /// That test proves the ack text *resolves* through the catalogue, but it
    /// runs under whatever locale the test process picks — and `i18n`'s
    /// `LOCALE` is a process-wide `OnceLock` a test cannot re-set. Under `en`
    /// the catalogue value and the English literal are byte-identical, so a
    /// behavioural assertion alone cannot distinguish
    /// `get_required_cli_string("...-denied")` from `"Denied"`. It catches a
    /// wrong or missing key; it does not catch a literal.
    ///
    /// This closes that specific hole at the source level: every arm of the
    /// ack `match` must go through `i18n::get_required_cli_string`, and no
    /// arm may carry a bare English literal. Together the two tests pin the
    /// catalogue contract at both the behavioural and source level, so a
    /// future move of this block cannot silently re-hard-code the strings.
    #[test]
    fn callback_ack_arms_are_all_catalogue_lookups() {
        let region = callback_ack_source_region();

        for key in [
            "channel-telegram-approval-ack-approved",
            "channel-telegram-approval-ack-always-approved",
            "channel-telegram-approval-ack-denied",
            "channel-telegram-approval-ack-unknown",
            "channel-telegram-approval-ack-already-resolved",
        ] {
            assert!(
                region.contains(&format!(
                    "i18n::get_required_cli_string(\n                            \"{key}\"\n"
                )) || region.contains(&format!("i18n::get_required_cli_string(\"{key}\")")),
                "ack arm for `{key}` must be a Fluent catalogue lookup, not a literal"
            );
        }

        // Exactly six lookups across the five arms: the four action acks,
        // the already-resolved toast, and the unknown-action fallback that
        // serves both the won-branch fallthrough and the no-response case.
        // An arm added or converted to a literal breaks this.
        assert_eq!(
            region.matches("i18n::get_required_cli_string").count(),
            6,
            "every arm of the ack match must resolve through the Fluent catalogue"
        );

        // The localization regression in literal form: no bare English ack word may
        // appear in this region (the emoji prefixes are protocol, not prose).
        for literal in [
            "\"Approved\"",
            "\"Always approved\"",
            "\"Denied\"",
            "\"Unknown action\"",
            "❌ Denied",
            "✅ Approved",
        ] {
            assert!(
                !region.contains(literal),
                "hard-coded English ack text `{literal}` reappeared in the callback arm; \
                 #9517 localized these through the Fluent catalogue"
            );
        }
    }

    /// A `getFile` failure must carry Telegram's own diagnostics and be
    /// classified conservatively. Only a confidently permanent vendor
    /// rejection may be `Permanent`; everything else retries, because
    /// retrying a recoverable failure is safe while skipping one loses a
    /// message.
    #[test]
    fn get_file_failures_classify_permanent_vendor_rejections_only() {
        use reqwest::StatusCode;

        // Telegram's real shape for an invalid/expired file id: HTTP 200
        // with an `ok: false` envelope. This is the case that used to
        // head-of-line block forever.
        let expired = serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: invalid file_id",
        });
        let e = FileLookupError::classify(StatusCode::OK, Some(&expired));
        assert_eq!(
            e.kind,
            FileLookupFailure::Permanent,
            "an ok:false 400 is permanent: {e}"
        );
        // The vendor evidence must survive into the message.
        assert!(e.message.contains("400"), "error_code missing: {e}");
        assert!(
            e.message.contains("invalid file_id"),
            "description missing: {e}"
        );

        // File too big — also permanent.
        let too_big = serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: file is too big",
        });
        assert_eq!(
            FileLookupError::classify(StatusCode::OK, Some(&too_big)).kind,
            FileLookupFailure::Permanent
        );

        // Forbidden — permanent.
        let forbidden = serde_json::json!({
            "ok": false,
            "error_code": 403,
            "description": "Forbidden: bot was blocked by the user",
        });
        assert_eq!(
            FileLookupError::classify(StatusCode::FORBIDDEN, Some(&forbidden)).kind,
            FileLookupFailure::Permanent
        );

        // 429 is a rate limit: retryable despite being 4xx.
        let rate_limited = serde_json::json!({
            "ok": false,
            "error_code": 429,
            "description": "Too Many Requests: retry after 30",
        });
        assert_eq!(
            FileLookupError::classify(StatusCode::TOO_MANY_REQUESTS, Some(&rate_limited)).kind,
            FileLookupFailure::Transient,
            "429 must stay transient"
        );

        // 5xx is an outage: retryable.
        let outage = serde_json::json!({
            "ok": false,
            "error_code": 500,
            "description": "Internal Server Error",
        });
        assert_eq!(
            FileLookupError::classify(StatusCode::INTERNAL_SERVER_ERROR, Some(&outage)).kind,
            FileLookupFailure::Transient
        );

        // A malformed body with no usable envelope: unrecognised, so
        // transient. Never guess permanence.
        let malformed = serde_json::json!({"unexpected": "shape"});
        assert_eq!(
            FileLookupError::classify(StatusCode::OK, Some(&malformed)).kind,
            FileLookupFailure::Transient
        );
        assert_eq!(
            FileLookupError::classify(StatusCode::OK, None).kind,
            FileLookupFailure::Transient
        );

        // A body-less 4xx carries no vendor evidence at all. It may come
        // from an intermediary rather than the Bot API, so it must never be
        // acknowledged as a terminal rejection.
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::REQUEST_TIMEOUT,
        ] {
            assert_eq!(
                FileLookupError::classify(status, None).kind,
                FileLookupFailure::Transient,
                "a body-less {status} must stay transient"
            );
        }

        // 408 is retryable per RFC 9110, even when the vendor names it.
        let timeout = serde_json::json!({
            "ok": false,
            "error_code": 408,
            "description": "Request Timeout",
        });
        assert_eq!(
            FileLookupError::classify(StatusCode::REQUEST_TIMEOUT, Some(&timeout)).kind,
            FileLookupFailure::Transient,
            "408 must stay transient"
        );

        // A 4xx whose body is malformed gives no structured evidence.
        let malformed_4xx = serde_json::json!({"unexpected": "shape"});
        assert_eq!(
            FileLookupError::classify(StatusCode::BAD_REQUEST, Some(&malformed_4xx)).kind,
            FileLookupFailure::Transient,
            "a malformed 4xx body must stay transient"
        );

        // `ok: false` without an `error_code` is still unstructured: the
        // reason is unknown, so permanence cannot be inferred.
        let no_code = serde_json::json!({
            "ok": false,
            "description": "Bad Request: something",
        });
        assert_eq!(
            FileLookupError::classify(StatusCode::BAD_REQUEST, Some(&no_code)).kind,
            FileLookupFailure::Transient,
            "ok:false without an error_code must stay transient"
        );

        // State-dependent 4xx codes are recoverable by definition: the same
        // request can succeed once the conflicting state clears (409) or the
        // early request is replayed (425). Acknowledging them would discard
        // an update whose download could still succeed.
        for (code, description) in [
            (409, "Conflict: terminated by other getUpdates request"),
            (425, "Too Early: retry the request"),
        ] {
            let state_dependent = serde_json::json!({
                "ok": false,
                "error_code": code,
                "description": description,
            });
            assert_eq!(
                FileLookupError::classify(StatusCode::OK, Some(&state_dependent)).kind,
                FileLookupFailure::Transient,
                "a state-dependent {code} must stay retryable"
            );
        }

        // Codes outside the substantiated terminal allowlist — including
        // deployment-wide failures and codes Telegram may introduce later —
        // stay transient. The Bot API documents `error_code` contents as
        // subject to change, so an unrecognised structured code is not proof
        // that the lookup can never succeed.
        for code in [401, 402, 404, 405, 410, 418, 422, 451, 499] {
            let unrecognised = serde_json::json!({
                "ok": false,
                "error_code": code,
                "description": "Unrecognised structured rejection",
            });
            assert_eq!(
                FileLookupError::classify(StatusCode::OK, Some(&unrecognised)).kind,
                FileLookupFailure::Transient,
                "an unrecognised structured {code} must stay retryable"
            );
        }
    }

    /// End-to-end proof of the liveness property: an update whose file id
    /// Telegram permanently rejects must be acknowledged, so a later update
    /// behind it in the ordered batch is still delivered.
    ///
    /// Before the classification, `getFile` mapped every failure to
    /// `RetryTransient`, so this update pinned the offset and the message
    /// behind it could never arrive.
    #[tokio::test]
    async fn listen_permanently_rejected_file_id_does_not_block_later_updates() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid_bad = 8_000;
        let uid_good = 8_001;
        let bad = telegram_document_update(uid_bad, 60, 222, "alice", "expired999", "gone.pdf");
        let good = telegram_text_update(uid_good, 61, 222, "alice", "i am behind the bad one");

        mount_telegram_get_updates(&mock_server, 0, serde_json::json!([bad, good])).await;
        mount_telegram_get_updates(&mock_server, uid_good + 1, serde_json::json!([])).await;

        // Telegram's real permanent-rejection shape: 200 OK, ok:false, 400.
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: invalid file_id",
            })))
            .mount(&mock_server)
            .await;

        let workspace = tempfile::tempdir().unwrap();
        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri())
            .with_workspace_dir(workspace.path().to_path_buf()),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // The update behind the permanently rejected one must arrive.
        let msg = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out: a permanently rejected file id head-of-line blocked the batch")
            .expect("channel closed before delivering the update behind the rejected one");
        assert_eq!(msg.content, "i am behind the bad one");

        telegram_expect_main_loop_offset(
            &mock_server,
            uid_good + 1,
            LISTEN_HANG_GUARD,
            "past the permanently rejected update",
        )
        .await;

        handle.abort();
    }

    /// The other half of the contract: a *transient* `getFile` failure must
    /// still pin the offset. Classification must not become a blanket
    /// "acknowledge on any error", which would reintroduce message loss.
    #[tokio::test]
    async fn listen_transient_file_failure_still_holds_the_offset() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid = 8_100;
        let doc = telegram_document_update(uid, 70, 333, "alice", "flaky999", "later.pdf");

        mount_telegram_get_updates(&mock_server, 0, serde_json::json!([doc])).await;
        mount_telegram_get_updates(&mock_server, uid + 1, serde_json::json!([])).await;

        // 500 twice (transient), then success — the offset must stay at 0
        // across the failures and only advance once the download works.
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"file_path": "documents/later.pdf"}
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/file/bot[^/]+/.*$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pdf bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let workspace = tempfile::tempdir().unwrap();
        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri())
            .with_workspace_dir(workspace.path().to_path_buf()),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // The update is retried, not skipped, and eventually delivered.
        let msg = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out: a transient failure was wrongly skipped instead of retried")
            .expect("channel closed before delivering the retried attachment");
        assert!(
            msg.content.contains("later.pdf"),
            "unexpected content: {}",
            msg.content
        );

        // More than one poll at the un-advanced offset proves it was held.
        let held_polls = telegram_main_loop_getupdates_bodies(&mock_server)
            .await
            .iter()
            .filter(|b| b.get("offset").and_then(serde_json::Value::as_i64) == Some(0))
            .count();
        assert!(
            held_polls >= 2,
            "a transient failure must hold the offset for a retry, saw {held_polls} poll(s) at 0"
        );

        handle.abort();
    }

    /// The regression for the unknown-4xx loss path.
    ///
    /// A body-less HTTP 408 carries no vendor evidence of a terminal
    /// rejection: RFC 9110 §15.5.9 permits retrying it, and an intermediary
    /// can emit one without the Bot API being involved. Classifying the whole
    /// non-429 4xx class as permanent acknowledged it, advancing the offset
    /// and silently consuming the very update this path exists to preserve.
    ///
    /// Proves the update is held rather than acknowledged, and is still
    /// delivered once the transient condition clears.
    #[tokio::test]
    async fn listen_bodyless_408_holds_the_offset_and_later_recovers() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid = 8_200;
        let doc = telegram_document_update(uid, 80, 444, "alice", "timeout999", "held.pdf");

        mount_telegram_get_updates(&mock_server, 0, serde_json::json!([doc])).await;
        mount_telegram_get_updates(&mock_server, uid + 1, serde_json::json!([])).await;

        // A bare 408 with no body at all: no `ok`, no `error_code`.
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(408))
            .up_to_n_times(2)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"file_path": "documents/held.pdf"}
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/file/bot[^/]+/.*$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pdf bytes".to_vec()))
            .mount(&mock_server)
            .await;

        let workspace = tempfile::tempdir().unwrap();
        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["alice".to_string()]),
                false,
            )
            .with_api_base(mock_server.uri())
            .with_workspace_dir(workspace.path().to_path_buf()),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let listen_ch = ch.clone();
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        // The update must survive the 408s and arrive after recovery.
        let msg = tokio::time::timeout(LISTEN_HANG_GUARD, rx.recv())
            .await
            .expect("timed out: a body-less 408 was acknowledged instead of retried")
            .expect("channel closed: the update behind a 408 was silently consumed");
        assert!(
            msg.content.contains("held.pdf"),
            "unexpected content: {}",
            msg.content
        );

        // More than one poll at offset 0 proves the update was held, not
        // acknowledged past.
        let held_polls = telegram_main_loop_getupdates_bodies(&mock_server)
            .await
            .iter()
            .filter(|b| b.get("offset").and_then(serde_json::Value::as_i64) == Some(0))
            .count();
        assert!(
            held_polls >= 2,
            "a body-less 408 must hold the offset for a retry, saw {held_polls} poll(s) at 0"
        );

        handle.abort();
    }
    /// An unauthorized-sender VOICE update must be acknowledged like any
    /// other permanent skip — the voice parser rejects it before any
    /// download, the attachment parser does not match voice payloads, the
    /// offset still ends up past it and the authorized update behind it —
    /// and, since media carries no top-level `text`, the sender must still
    /// get the same "requires operator approval" notice a text sender gets.
    #[tokio::test]
    async fn listen_acknowledges_unauthorized_voice_update_without_download() {
        let voice_update =
            telegram_voice_update(5_000, 30, 999, "zeroclaw_unauthorized", "voice789");
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            max_duration_secs: 120,
            ..Default::default()
        };
        assert_listen_skips_unauthorized_update(voice_update, |ch| ch.with_transcription(tc), 1)
            .await;
    }

    /// An unauthorized-sender DOCUMENT update must get the same treatment
    /// as voice: no `getFile` download, the offset advances past it, it is
    /// never delivered to `tx`, and — the point of this fix — the sender
    /// still receives the unauthorized-approval notice even though the
    /// update carries no top-level `text`, only a `document` payload.
    #[tokio::test]
    async fn listen_notifies_unauthorized_document_update_without_download() {
        let document_update = telegram_document_update(
            6_000,
            40,
            1_010,
            "zeroclaw_unauthorized",
            "file999",
            "report.pdf",
        );
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_path_buf();
        assert_listen_skips_unauthorized_update(
            document_update,
            |ch| ch.with_workspace_dir(workspace_path),
            1,
        )
        .await;
    }

    /// An unauthorized-sender VOICE update whose duration exceeds the
    /// transcription config's `max_duration_secs` must be dropped exactly
    /// like an authorized sender's identical over-duration voice note:
    /// `try_parse_voice_message` bails on it permanently before
    /// `handle_unauthorized_message` is even reached, so
    /// `message_exceeds_parser_limits` must keep it out of the approval
    /// notice too — no download, no notice, offset still advances.
    #[tokio::test]
    async fn listen_skips_unauthorized_over_duration_voice_update_without_notice() {
        let mut voice_update =
            telegram_voice_update(5_100, 31, 1_020, "zeroclaw_unauthorized", "voice999");
        voice_update["message"]["voice"]["duration"] = serde_json::json!(200);
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            max_duration_secs: 120,
            ..Default::default()
        };
        assert_listen_skips_unauthorized_update(voice_update, |ch| ch.with_transcription(tc), 0)
            .await;
    }

    /// An unauthorized-sender DOCUMENT update whose size exceeds
    /// `TELEGRAM_MAX_FILE_DOWNLOAD_BYTES` must be dropped exactly like an
    /// authorized sender's identical oversized attachment:
    /// `try_parse_attachment_message` bails on it permanently before
    /// `handle_unauthorized_message` is even reached, so
    /// `message_exceeds_parser_limits` must keep it out of the approval
    /// notice too — no download, no notice, offset still advances.
    #[tokio::test]
    async fn listen_skips_unauthorized_oversized_document_update_without_notice() {
        let mut document_update = telegram_document_update(
            6_100,
            41,
            1_030,
            "zeroclaw_unauthorized",
            "file000",
            "huge.pdf",
        );
        document_update["message"]["document"]["file_size"] =
            serde_json::json!(TELEGRAM_MAX_FILE_DOWNLOAD_BYTES + 1);
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_path_buf();
        assert_listen_skips_unauthorized_update(
            document_update,
            |ch| ch.with_workspace_dir(workspace_path),
            0,
        )
        .await;
    }

    /// Updates without any content the bot could process for an
    /// authorized sender — stickers, service messages like
    /// `new_chat_members` — must stay silent even from unauthorized
    /// senders: no notice, no download, offset still advances. Without
    /// this gate every join/leave/pin by a non-allowlisted group member
    /// would spam the chat with approval notices.
    #[tokio::test]
    async fn listen_stays_silent_for_unauthorized_update_without_processable_content() {
        let sticker_update = serde_json::json!({
            "update_id": 7_000,
            "message": {
                "message_id": 60,
                "chat": {"id": 1_020, "type": "private"},
                "from": {"id": 160_000, "username": "zeroclaw_unauthorized"},
                "sticker": {"file_id": "sticker123", "width": 512, "height": 512},
            }
        });
        assert_listen_skips_unauthorized_update(sticker_update, |ch| ch, 0).await;

        let service_update = serde_json::json!({
            "update_id": 7_100,
            "message": {
                "message_id": 61,
                "chat": {"id": 1_030, "type": "group"},
                "from": {"id": 161_000, "username": "zeroclaw_unauthorized"},
                "new_chat_members": [{"id": 161_000, "username": "zeroclaw_unauthorized"}],
            }
        });
        assert_listen_skips_unauthorized_update(service_update, |ch| ch, 0).await;
    }

    /// Payloads whose *shape* the canonical parsers reject must not draw a
    /// notice either, even when the deployment is fully configured for that
    /// media kind. `message_has_processable_content` resolves acceptance
    /// through `text.as_str()`, `parse_voice_metadata`, and
    /// `parse_attachment_metadata` rather than raw JSON key presence, so a
    /// null `text`, an empty `voice`/`document` object, or an empty `photo`
    /// array is treated exactly as it would be for an authorized sender:
    /// dropped as a permanent skip with no notice, no download, and no
    /// dispatch, while the offset still advances past it.
    ///
    /// Telegram response JSON is an external trust boundary, so its shape
    /// is validated before the notice behavior is triggered.
    #[tokio::test]
    async fn listen_stays_silent_for_unauthorized_media_the_parsers_would_reject() {
        fn malformed_update(
            update_id: i64,
            message_id: i64,
            chat_id: i64,
            payload: serde_json::Value,
        ) -> serde_json::Value {
            let mut message = serde_json::json!({
                "message_id": message_id,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": 162_000, "username": "zeroclaw_unauthorized"},
            });
            let serde_json::Value::Object(fields) = payload else {
                unreachable!("malformed payload fixture must be a JSON object");
            };
            for (key, value) in fields {
                message[key] = value;
            }
            serde_json::json!({"update_id": update_id, "message": message})
        }

        fn transcription_config() -> zeroclaw_config::schema::TranscriptionConfig {
            zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                api_key: Some("test_key".to_string()),
                max_duration_secs: 120,
                ..Default::default()
            }
        }

        // `text` present but not a string — `parse_update_message` bails on
        // `as_str()`, so an authorized sender's identical update is dropped.
        let null_text = malformed_update(7_400, 64, 1_060, serde_json::json!({"text": null}));
        assert_listen_skips_unauthorized_update(null_text, |ch| ch, 0).await;

        // `voice` present but carrying no `file_id` — `parse_voice_metadata`
        // returns `None` even with transcription fully configured.
        let empty_voice = malformed_update(7_500, 65, 1_070, serde_json::json!({"voice": {}}));
        assert_listen_skips_unauthorized_update(
            empty_voice,
            |ch| ch.with_transcription(transcription_config()),
            0,
        )
        .await;

        // `document` present but carrying no `file_id` —
        // `parse_attachment_metadata` returns `None` even with a workspace dir.
        let empty_document =
            malformed_update(7_600, 66, 1_080, serde_json::json!({"document": {}}));
        let document_workspace = tempfile::tempdir().unwrap();
        let document_workspace_path = document_workspace.path().to_path_buf();
        assert_listen_skips_unauthorized_update(
            empty_document,
            |ch| ch.with_workspace_dir(document_workspace_path),
            0,
        )
        .await;

        // Empty `photo` array — `parse_attachment_metadata` bails on
        // `photos.last()`, so there is no highest-resolution size to download.
        let empty_photo = malformed_update(7_700, 67, 1_090, serde_json::json!({"photo": []}));
        let photo_workspace = tempfile::tempdir().unwrap();
        let photo_workspace_path = photo_workspace.path().to_path_buf();
        assert_listen_skips_unauthorized_update(
            empty_photo,
            |ch| ch.with_workspace_dir(photo_workspace_path),
            0,
        )
        .await;
    }

    /// Media the deployment is not configured to process must not draw a
    /// notice either: with transcription unconfigured the voice parser
    /// would drop the update even from an authorized sender, so telling
    /// an unauthorized one to "send your message again" after approval
    /// would promise processing that cannot happen. Same for documents
    /// without a workspace dir. The undecorated helper channel has
    /// neither configured.
    #[tokio::test]
    async fn listen_stays_silent_for_unauthorized_media_the_deployment_cannot_process() {
        let voice_update =
            telegram_voice_update(7_200, 62, 1_040, "zeroclaw_unauthorized", "voice321");
        assert_listen_skips_unauthorized_update(voice_update, |ch| ch, 0).await;

        let document_update = telegram_document_update(
            7_300,
            63,
            1_050,
            "zeroclaw_unauthorized",
            "file222",
            "notes.pdf",
        );
        assert_listen_skips_unauthorized_update(document_update, |ch| ch, 0).await;
    }

    /// A captioned media update from an unauthorized sender must have its
    /// `caption` treated the same as a text sender's `text` — specifically,
    /// a `/bind <code>` in the caption must reach `extract_bind_code` and
    /// take the pairing branch, not the plain unauthorized-approval notice.
    /// Exercised directly against `handle_unauthorized_message` as
    /// lower-level coverage that complements the listener-level regression
    /// `listen_routes_captioned_bind_on_oversized_document_without_download`:
    /// this pins the caption-to-pairing branch in isolation, while that one
    /// proves the same behavior through the real poll/parser/authorization
    /// path.
    #[tokio::test]
    async fn handle_unauthorized_message_reads_bind_code_from_caption() {
        use wiremock::MockServer;

        let mock_server = MockServer::start().await;
        mount_telegram_send_message_ok(&mock_server, 1, &[r#""chat_id":"2020""#]).await;

        // An empty peer list auto-provisions an active pairing guard (with
        // a random 6-digit code), the same precondition the pairing branch
        // needs. The workspace dir makes document updates processable, so
        // the content gate lets the captioned update through.
        let workspace = tempfile::tempdir().unwrap();
        let ch = TelegramChannel::new(
            "test-token".into(),
            "telegram_test_alias",
            Arc::new(Vec::new),
            false,
        )
        .with_api_base(mock_server.uri())
        .with_workspace_dir(workspace.path().to_path_buf());

        let mut update = telegram_document_update(
            9_000,
            50,
            2_020,
            "zeroclaw_unauthorized",
            "file111",
            "notes.pdf",
        );
        // Generated pairing codes are always exactly six digits, so a
        // non-digit code can never accidentally match and the invalid-code
        // branch is deterministic.
        update["message"]["caption"] = serde_json::json!("/bind not-a-real-code");

        ch.handle_unauthorized_message(&update).await;

        let send_message_bodies: Vec<serde_json::Value> = mock_server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.url.path().ends_with("/sendMessage"))
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect();
        assert_eq!(
            send_message_bodies.len(),
            1,
            "expected exactly one sendMessage request, got: {send_message_bodies:?}"
        );
        let sent_text = send_message_bodies[0]
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(
            sent_text.contains("Invalid binding code"),
            "expected the pairing branch's wrong-code reply from a captioned /bind, got: {sent_text}"
        );
    }

    /// Pin the real poll/parser/authorization path for captioned pairing.
    /// Even though this document exceeds the normal attachment-size limit,
    /// `/bind` is handled before media eligibility: the listener must send
    /// the deterministic invalid-code response without downloading or
    /// delivering the document, then advance past the permanent skip.
    #[tokio::test]
    async fn listen_routes_captioned_bind_on_oversized_document_without_download() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        mount_telegram_startup_probe(&mock_server).await;

        let uid = 9_100;
        let chat_id = 2_120;
        let mut update = telegram_document_update(
            uid,
            51,
            chat_id,
            "zeroclaw_unauthorized",
            "file112",
            "huge.pdf",
        );
        update["message"]["document"]["file_size"] =
            serde_json::json!(TELEGRAM_MAX_FILE_DOWNLOAD_BYTES + 1);
        update["message"]["caption"] = serde_json::json!("/bind not-a-real-code");

        mount_telegram_get_updates(&mock_server, 0, serde_json::json!([update])).await;
        mount_telegram_get_updates(&mock_server, uid + 1, serde_json::json!([])).await;
        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"file_path": "unauthorized/never-downloaded"}
            })))
            .expect(0)
            .mount(&mock_server)
            .await;
        let chat_fragment = format!(r#""chat_id":"{chat_id}""#);
        mount_telegram_send_message_ok(
            &mock_server,
            1,
            &[chat_fragment.as_str(), "Invalid binding code"],
        )
        .await;

        let workspace = tempfile::tempdir().unwrap();
        let ch = Arc::new(
            TelegramChannel::new(
                "test-token".into(),
                "telegram_test_alias",
                Arc::new(Vec::new),
                false,
            )
            .with_api_base(mock_server.uri())
            .with_workspace_dir(workspace.path().to_path_buf()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let listen_ch = Arc::clone(&ch);
        let handle = zeroclaw_spawn::spawn!(async move { listen_ch.listen(tx).await });

        telegram_expect_main_loop_offset(
            &mock_server,
            uid + 1,
            LISTEN_HANG_GUARD,
            "past the captioned pairing update",
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), rx.recv())
                .await
                .is_err(),
            "unauthorized oversized document unexpectedly reached channel dispatch"
        );

        handle.abort();
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

    // ── Attachment content format tests ──────────────────────────────

    /// Build a typed envelope for content-format tests.
    fn envelope(
        file_name: &str,
        mime_type: Option<&str>,
        data: &[u8],
    ) -> zeroclaw_api::media::MediaAttachment {
        zeroclaw_api::media::MediaAttachment {
            file_name: file_name.to_string(),
            data: data.to_vec(),
            mime_type: mime_type.map(str::to_string),
            marker: None,
        }
    }

    /// Photo attachments with image extension must use `[IMAGE:/path]` marker
    /// so the multimodal pipeline validates vision capability on the model_provider.
    #[test]
    fn attachment_photo_content_uses_image_marker() {
        let local_path = std::path::Path::new("/tmp/workspace/photo_123_45.jpg");

        let content =
            format_attachment_content(&envelope("photo_123_45.jpg", None, &[]), local_path);

        assert_eq!(content, "[IMAGE:/tmp/workspace/photo_123_45.jpg]");
        assert!(content.starts_with("[IMAGE:"));
        assert!(content.ends_with(']'));
    }

    #[test]
    fn attachment_document_content_uses_document_label() {
        let local_path = std::path::Path::new("/tmp/workspace/report.pdf");

        let content = format_attachment_content(
            &envelope("report.pdf", Some("application/pdf"), &[]),
            local_path,
        );

        assert_eq!(content, "[Document: report.pdf] /tmp/workspace/report.pdf");
        assert!(!content.contains("[IMAGE:"));
    }

    /// An image sent "as file" must get the `[IMAGE:]` marker even without an
    /// image extension, as long as the loader can resolve the payload to a
    /// format it accepts. The marker keeps the media pipeline from re-inlining
    /// the same image as base64.
    #[test]
    fn image_document_content_uses_image_marker() {
        let local_path = std::path::Path::new("/tmp/workspace/telegram_files/upload");

        // Magic bytes only: no MIME, no extension. The loader sniffs the same
        // bytes and reaches the same verdict.
        let content = format_attachment_content(
            &envelope("upload", None, &[0xFF, 0xD8, 0xFF, 0xE0]),
            local_path,
        );
        assert_eq!(content, "[IMAGE:/tmp/workspace/telegram_files/upload]");

        // The sender's declared MIME travels with the envelope but the loader
        // never sees it for a path marker: it resolves extension then magic.
        // A declared type alone therefore cannot earn a marker the loader
        // would reject.
        let content = format_attachment_content(
            &envelope("upload", Some("image/jpeg"), b"not actually an image"),
            local_path,
        );
        assert!(
            content.starts_with("[Document:"),
            "a declared MIME must not outvote the loader's own resolution: {content}"
        );
    }

    /// Formats the multimodal loader cannot normalize must stay documents.
    /// Marking them would be strictly worse than not marking them: preparation
    /// drops the rejected marker for a "could not be loaded" note, and the
    /// `[Document:]` line that would have kept the saved path reachable was
    /// never emitted, so both the bytes and the path are lost.
    #[test]
    fn unloadable_image_formats_stay_documents() {
        for (filename, data) in [
            ("photo.heic", &b"\x00\x00\x00\x18ftypheic"[..]),
            ("scan.tiff", &b"\x49\x49\x2a\x00rest"[..]),
            (
                "logo.svg",
                &b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"[..],
            ),
            ("old.bmp", &b"BMxxxx"[..]),
        ] {
            let path_str = format!("/tmp/ws/{filename}");
            let path = std::path::Path::new(&path_str);
            let content = format_attachment_content(&envelope(filename, None, data), path);
            assert!(
                content.starts_with("[Document:"),
                "{filename}: unloadable image must stay a document, got: {content}"
            );
            assert!(
                content.contains(&path_str),
                "{filename}: the saved path must remain reachable, got: {content}"
            );
        }
    }

    /// An extensionless upload whose magic bytes are an unloadable format is
    /// rejected the same way, so sniffing cannot smuggle one past the gate.
    #[test]
    fn unloadable_magic_bytes_stay_documents() {
        let path = std::path::Path::new("/tmp/ws/upload");
        let content =
            format_attachment_content(&envelope("upload", None, b"\x49\x49\x2a\x00rest"), path);
        assert!(
            content.starts_with("[Document:"),
            "TIFF magic must not earn an image marker: {content}"
        );
    }

    /// A `.md` upload is text, so no envelope signal ever classifies it as an
    /// image and it must never produce an `[IMAGE:]` marker.
    #[test]
    fn markdown_file_never_produces_image_marker() {
        let local_path = std::path::Path::new("/tmp/workspace/telegram_files/notes.md");

        // No envelope signal says image, so even a Telegram misclassification
        // (photo vs document) cannot produce an [IMAGE:] marker: the verdict
        // comes from the envelope, not from Telegram's kind.
        let content = format_attachment_content(
            &envelope("notes.md", None, b"# heading\nbody text"),
            local_path,
        );
        assert!(
            !content.contains("[IMAGE:"),
            "markdown must not get [IMAGE:] marker: {content}"
        );
        assert!(content.starts_with("[Document:"));
    }

    /// Non-image files fall back to `[Document:]` format regardless of how
    /// Telegram classified them (the envelope decides, not the kind).
    #[test]
    fn non_image_attachment_falls_back_to_document_format() {
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
            let content =
                format_attachment_content(&envelope(filename, None, b"not image bytes"), path);
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

    /// Every extension the multimodal loader accepts produces an `[IMAGE:]`
    /// marker. The list is exactly the loader's, not a wider "looks like an
    /// image" set — see `unloadable_image_formats_stay_documents`.
    #[test]
    fn image_extensions_produce_image_marker() {
        for ext in ["png", "jpg", "jpeg", "gif", "webp"] {
            let filename = format!("photo_1_2.{ext}");
            let path_str = format!("/tmp/ws/{filename}");
            let path = std::path::Path::new(&path_str);
            let content = format_attachment_content(&envelope(&filename, None, &[]), path);
            assert!(
                content.starts_with("[IMAGE:"),
                "{ext}: image should get [IMAGE:] marker, got: {content}"
            );
        }
    }

    #[test]
    fn markdown_attachment_not_detected_by_multimodal_image_markers() {
        let content = format_attachment_content(
            &envelope("notes.md", None, b"# heading"),
            std::path::Path::new("/tmp/ws/notes.md"),
        );
        let messages = vec![zeroclaw_providers::ChatMessage::user(content)];
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&messages),
            0,
            "markdown file must not trigger image marker detection"
        );
    }

    /// `count_image_markers` from the multimodal module must detect the
    /// `[IMAGE:]` marker produced by photo attachment formatting.
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

        let doc_content = format_attachment_content(
            &envelope(doc_filename, Some("application/pdf"), b"%PDF-1.4 fake"),
            &doc_path,
        );
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

        let photo_bytes = std::fs::read(&photo_path).expect("read photo fixture");
        let photo_content =
            format_attachment_content(&envelope(photo_filename, None, &photo_bytes), &photo_path);
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
        let md_content = format_attachment_content(
            &envelope(md_filename, None, b"# Hello\nSome markdown"),
            &md_path,
        );
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

    /// The 6 built-in Telegram command entries, resolved through the i18n
    /// catalog exactly as production's `register_bot_commands` does. Shared
    /// by every `register_bot_commands_*` test so expectations stay in sync
    /// with production ordering/content regardless of the active locale.
    fn expected_builtin_command_json() -> Vec<serde_json::Value> {
        let entries = [
            ("new", "channel-telegram-cmd-new-desc"),
            ("clear", "channel-telegram-cmd-clear-desc"),
            ("stop", "channel-telegram-cmd-stop-desc"),
            ("model", "channel-telegram-cmd-model-desc"),
            ("models", "channel-telegram-cmd-models-desc"),
            ("config", "channel-telegram-cmd-config-desc"),
        ];
        entries
            .into_iter()
            .map(|(command, key)| {
                let description = zeroclaw_runtime::i18n::get_required_cli_string(key);
                assert!(
                    !description.starts_with('{'),
                    "description for /{command} resolved to the missing-key sentinel: {description}"
                );
                serde_json::json!({ "command": command, "description": description })
            })
            .collect()
    }

    #[tokio::test]
    async fn register_bot_commands_sends_correct_payload() {
        use wiremock::matchers::{body_json, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        let expected_body = serde_json::json!({
            "commands": expected_builtin_command_json()
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

    #[test]
    fn register_bot_commands_sends_independently_pinned_french_payload() {
        // Locale selection is process-global and immutable after its first
        // lookup. Run the ignored helper in a fresh process so `init("fr")`
        // deterministically owns that first lookup without racing unrelated
        // tests in this binary. An isolated config directory also prevents a
        // developer-installed disk catalog from overriding the committed
        // French source that this boundary regression is intended to prove.
        let config_dir = tempfile::tempdir().expect("create isolated locale config directory");
        let mut command = std::process::Command::new(
            std::env::current_exe().expect("current test executable should be available"),
        );
        command
            .args([
                "register_bot_commands_french_payload_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("ZEROCLAW_CONFIG_DIR", config_dir.path())
            .env_remove("ZEROCLAW_DATA_DIR")
            .env_remove("ZEROCLAW_WORKSPACE");
        let output = command
            .output()
            .expect("French command-menu child test should start");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "French command-menu child test failed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr
        );
        assert!(
            stdout.contains("register_bot_commands_french_payload_helper ... ok"),
            "French command-menu helper did not run\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    #[tokio::test]
    #[ignore = "subprocess helper for process-global French locale"]
    async fn register_bot_commands_french_payload_helper() {
        zeroclaw_runtime::i18n::init("fr");

        use wiremock::matchers::{body_json, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let expected_body = serde_json::json!({
            "commands": [
                { "command": "new", "description": "Démarrer une nouvelle session de conversation" },
                { "command": "clear", "description": "Effacer cette session de conversation" },
                { "command": "stop", "description": "Annuler la tâche en cours" },
                { "command": "model", "description": "Afficher ou changer le modèle actuel" },
                { "command": "models", "description": "Lister les fournisseurs de modèles disponibles ou changer de fournisseur" },
                { "command": "config", "description": "Afficher la configuration actuelle" },
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

        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
        )
        .with_api_base(mock_server.uri());

        ch.register_bot_commands().await;
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

    /// Build `n` commands whose descriptions sit at the per-command cap. This is the
    /// shape a real install reaches once enough tools and skills expose commands.
    fn telegram_commands_fixture(n: usize, description_len: usize) -> Vec<serde_json::Value> {
        (0..n)
            .map(|i| {
                serde_json::json!({
                    "command": format!("toolcmd{i:03}"),
                    "description": "d".repeat(description_len),
                })
            })
            .collect()
    }

    #[test]
    fn telegram_bot_commands_within_count_limit_can_still_exceed_the_body_budget() {
        // The exact shape the live API refuses: the count cap and the per-command
        // description cap are both satisfied, and the body is still too large.
        let commands = telegram_commands_fixture(
            TELEGRAM_MAX_BOT_COMMANDS,
            TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN,
        );
        assert_eq!(commands.len(), TELEGRAM_MAX_BOT_COMMANDS);
        assert!(
            telegram_bot_commands_body_len(&commands) > TELEGRAM_MAX_BOT_COMMANDS_BODY_BYTES,
            "a full command set at the description cap must exceed the body budget, \
             otherwise this test is not exercising the case the API rejects"
        );
    }

    #[test]
    fn fit_telegram_bot_commands_trims_an_oversized_body_to_the_budget() {
        let mut commands = telegram_commands_fixture(
            TELEGRAM_MAX_BOT_COMMANDS,
            TELEGRAM_COMMAND_DESCRIPTION_MAX_LEN,
        );
        let before = commands.len();
        let dropped = fit_telegram_bot_commands_to_body_budget(&mut commands);

        assert!(dropped > 0, "an oversized body must lose commands");
        assert_eq!(
            commands.len() + dropped,
            before,
            "every dropped command must be accounted for"
        );
        assert!(
            telegram_bot_commands_body_len(&commands) <= TELEGRAM_MAX_BOT_COMMANDS_BODY_BYTES,
            "the trimmed body must fit the budget"
        );
        assert!(
            !commands.is_empty(),
            "trimming must not empty the menu outright"
        );
    }

    #[test]
    fn fit_telegram_bot_commands_leaves_a_small_set_untouched() {
        // Over-correction control: a set that already fits must not be trimmed, so a
        // green result above cannot come from a function that always drops commands.
        let mut commands = telegram_commands_fixture(6, 40);
        let before = commands.clone();
        let dropped = fit_telegram_bot_commands_to_body_budget(&mut commands);

        assert_eq!(dropped, 0, "a body inside the budget must lose nothing");
        assert_eq!(
            commands, before,
            "a fitting set must be left byte-identical"
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
    async fn inbound_photo_populates_typed_image_attachment_envelope() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::media::MediaKind;

        let workspace = tempfile::tempdir().unwrap();
        let mock_server = MockServer::start().await;
        let photo_bytes: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x01, 0x02];

        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "file_path": "photos/file_1.jpg" }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/file/bot[^/]+/photos/file_1\.jpg$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(photo_bytes.clone()))
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

        let update = serde_json::json!({
            "message": {
                "message_id": 42,
                "chat": { "id": 123 },
                "from": { "username": "alice", "id": 99 },
                "photo": [
                    { "file_id": "small", "file_size": 10 },
                    { "file_id": "best", "file_size": 20 }
                ],
                "caption": "log this automatically"
            }
        });

        let msg = ch
            .try_parse_attachment_message(&update)
            .await
            .expect_parsed("photo update should parse into a channel message");

        // The typed envelope is the source of truth for image presence, so a
        // real photo must land here even though the marker is also emitted.
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].kind(), MediaKind::Image);
        assert_eq!(msg.attachments[0].data, photo_bytes);
        assert!(
            msg.content.contains("[IMAGE:"),
            "content marker must still be emitted for the multimodal pipeline: {}",
            msg.content
        );
    }

    #[tokio::test]
    async fn inbound_image_document_populates_image_kind_envelope() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::media::MediaKind;

        let workspace = tempfile::tempdir().unwrap();
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "file_path": "documents/file_7.jpg" }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/file/bot[^/]+/documents/file_7\.jpg$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x01u8, 0x02]))
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

        let update = serde_json::json!({
            "message": {
                "message_id": 43,
                "chat": { "id": 123 },
                "from": { "username": "alice", "id": 99 },
                "document": {
                    "file_id": "doc1",
                    "file_name": "menu.jpg",
                    "file_size": 2
                }
            }
        });

        let msg = ch
            .try_parse_attachment_message(&update)
            .await
            .expect_parsed("document update should parse into a channel message");

        // An image sent "as file" must classify as an image in the envelope,
        // so image-turn behavior cannot be sidestepped by attaching the photo
        // as a document.
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].kind(), MediaKind::Image);
        // And the content marker must match: an [IMAGE:] path marker, not
        // [Document:], so the media pipeline recognizes the file as already
        // marked instead of re-inlining it as base64.
        assert!(
            msg.content.contains("[IMAGE:"),
            "image document must get an [IMAGE:] marker: {}",
            msg.content
        );
    }

    #[tokio::test]
    async fn inbound_extensionless_document_carries_mime_and_flags_image() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let workspace = tempfile::tempdir().unwrap();
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path_regex(r"/bot[^/]+/getFile$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "file_path": "documents/file_8" }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"/file/bot[^/]+/documents/file_8$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xFFu8, 0xD8, 0xFF, 0xE0]))
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

        // An image uploaded as an extensionless document: no extension to
        // classify by, only the sender-declared MIME (and, failing that, the
        // payload's magic bytes). Both must reach the envelope so the image
        // gate cannot be dodged by stripping the file name.
        let update = serde_json::json!({
            "message": {
                "message_id": 44,
                "chat": { "id": 123 },
                "from": { "username": "alice", "id": 99 },
                "document": {
                    "file_id": "doc2",
                    "file_name": "upload",
                    "mime_type": "image/jpeg",
                    "file_size": 4
                }
            }
        });

        let msg = ch
            .try_parse_attachment_message(&update)
            .await
            .expect_parsed("document update should parse into a channel message");

        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].mime_type.as_deref(), Some("image/jpeg"));
        assert!(msg.attachments[0].looks_like_image());
        // Even without an extension, the envelope's image verdict must drive
        // the content marker so downstream marker-based consumers (media
        // pipeline dedup) agree with the typed envelope.
        assert!(
            msg.content.contains("[IMAGE:"),
            "extensionless image document must get an [IMAGE:] marker: {}",
            msg.content
        );
        assert!(
            msg.content.contains("upload"),
            "marker must carry the saved file path: {}",
            msg.content
        );
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

        let mut commands = expected_builtin_command_json();
        commands.push(
            serde_json::json!({ "command": "weather", "description": "Check the weather forecast" }),
        );
        let expected_body = serde_json::json!({ "commands": commands });

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

        let mut commands = expected_builtin_command_json();
        commands.push(serde_json::json!({ "command": "test_tool", "description": "A test tool" }));
        let expected_body = serde_json::json!({ "commands": commands });

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

    /// Scoped cleanup for the process-wide broadcast hook: clears the hook on
    /// drop so a panicking assertion cannot leak the installed hook into later
    /// tests. Declare after `__private_test_hook_lock()` so the clear runs
    /// while the hook lock is still held (guards drop in reverse declaration
    /// order).
    struct BroadcastHookGuard;

    impl Drop for BroadcastHookGuard {
        fn drop(&mut self) {
            zeroclaw_log::clear_broadcast_hook();
        }
    }

    #[tokio::test]
    async fn register_bot_commands_truncates_to_telegram_max() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Build enough tool specs to exceed the 100-command cap: 6 built-ins + 101 tools = 107.
        let specs: Vec<(String, String)> = (0..101)
            .map(|i| {
                (
                    format!("tool_{i:02}"),
                    format!("Description for tool {i:02}"),
                )
            })
            .collect();

        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_for_respond = captured.clone();

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/setMyCommands$"))
            .respond_with(move |req: &wiremock::Request| {
                let body = req.body_json::<serde_json::Value>().unwrap_or_default();
                *captured_for_respond.lock().unwrap() = Some(body);
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true, "result": true }))
            })
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
        .with_tool_command_specs(specs);

        // Install a broadcast hook so we can capture the WARN log event.
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        let _hook_cleanup = BroadcastHookGuard;
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        ch.register_bot_commands().await;

        let body = captured
            .lock()
            .unwrap()
            .take()
            .expect("setMyCommands body captured");
        let commands = body
            .get("commands")
            .and_then(|v| v.as_array())
            .expect("commands array");
        assert_eq!(
            commands.len(),
            TELEGRAM_MAX_BOT_COMMANDS,
            "must cap commands at {TELEGRAM_MAX_BOT_COMMANDS}, got {}",
            commands.len()
        );
        // Built-ins are registered first, followed by tools in input order.
        assert_eq!(commands[0]["command"], "new");
        assert_eq!(commands[5]["command"], "config");
        assert_eq!(commands[6]["command"], "tool_00");
        assert_eq!(
            commands[TELEGRAM_MAX_BOT_COMMANDS - 1]["command"],
            "tool_93",
            "last registered command must be the 94th tool (built-ins + 94 tools = 100)"
        );

        // Verify the WARN event: stable literal message identifies the event,
        // and the per-event counts ride solely in structured attributes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found_warn = false;
        while !found_warn && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| {
                            s.contains(
                                "Telegram command registration truncated to the platform limit",
                            )
                        })
                        .unwrap_or(false)
                    {
                        found_warn = true;
                        // Pin the structured attribute keys and values so that
                        // a silent rename or schema drift is caught.
                        let attrs = value.get("attributes");
                        assert!(
                            attrs.is_some(),
                            "WARN event must carry structured attributes"
                        );
                        let attrs = attrs.unwrap();
                        assert_eq!(
                            attrs
                                .get("TELEGRAM_MAX_BOT_COMMANDS")
                                .and_then(|v| v.as_u64()),
                            Some(TELEGRAM_MAX_BOT_COMMANDS as u64),
                            "structured attribute TELEGRAM_MAX_BOT_COMMANDS"
                        );
                        assert_eq!(
                            attrs.get("total_before_cap").and_then(|v| v.as_u64()),
                            Some(107),
                            "structured attribute total_before_cap"
                        );
                        assert_eq!(
                            attrs.get("registered").and_then(|v| v.as_u64()),
                            Some(100),
                            "structured attribute registered"
                        );
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_timeout) => break,
            }
        }
        assert!(
            found_warn,
            "truncation WARN must be captured with the stable literal message"
        );
    }

    /// The truncation WARN must carry channel attribution (`zeroclaw.channel`
    /// composite) when `register_bot_commands` runs under the attribution span
    /// the orchestrator opens around the supervised listener in
    /// `crates/zeroclaw-channels/src/orchestrator/mod.rs`.
    ///
    /// The event-level counters (`TELEGRAM_MAX_BOT_COMMANDS`, `total_before_cap`,
    /// `registered`) correctly live in `attributes`; the "who/where" identity
    /// (`channel = telegram.<alias>`) must ride in from the span, never from the
    /// call site. This pins the attribution/attrs split the logging contract
    /// requires: operators see the WARN attributed to the emitting Telegram
    /// channel, while the numeric counts remain in `attributes`.
    #[tokio::test]
    async fn register_bot_commands_truncation_warn_carries_channel_attribution() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_log::Instrument;

        let mock_server = MockServer::start().await;

        // 6 built-ins + 101 tools = 107, exceeding the 100-command cap.
        let specs: Vec<(String, String)> = (0..101)
            .map(|i| {
                (
                    format!("tool_{i:02}"),
                    format!("Description for tool {i:02}"),
                )
            })
            .collect();

        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/setMyCommands$"))
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
            "clamps",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        )
        .with_api_base(mock_server.uri())
        .with_tool_command_specs(specs);

        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        let _hook_cleanup = BroadcastHookGuard;
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        // Run under the same attribution span the orchestrator opens around
        // `listen()` — this is what carries the channel identity into the WARN.
        async {
            ch.register_bot_commands().await;
        }
        .instrument(zeroclaw_log::attribution_span!(&ch))
        .await;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found_warn = false;
        while !found_warn && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| {
                            s.contains(
                                "Telegram command registration truncated to the platform limit",
                            )
                        })
                        .unwrap_or(false)
                    {
                        found_warn = true;

                        // Attribution ("who/where") rides in from the span.
                        let zc = value
                            .get("zeroclaw")
                            .expect("WARN event must carry the zeroclaw attribution block");
                        assert_eq!(
                            zc.get("channel").and_then(|v| v.as_str()),
                            Some("telegram.clamps"),
                            "truncation WARN must be attributed to the emitting channel, got: {zc:?}"
                        );
                        assert_eq!(
                            zc.get("channel_type").and_then(|v| v.as_str()),
                            Some("telegram"),
                        );
                        assert_eq!(
                            zc.get("channel_alias").and_then(|v| v.as_str()),
                            Some("clamps"),
                        );

                        // Event-level counters stay in attrs, not attribution.
                        let attrs = value
                            .get("attributes")
                            .expect("WARN event must carry structured attributes");
                        assert_eq!(
                            attrs
                                .get("TELEGRAM_MAX_BOT_COMMANDS")
                                .and_then(|v| v.as_u64()),
                            Some(TELEGRAM_MAX_BOT_COMMANDS as u64),
                        );
                        assert_eq!(
                            attrs.get("total_before_cap").and_then(|v| v.as_u64()),
                            Some(107),
                        );
                        assert_eq!(attrs.get("registered").and_then(|v| v.as_u64()), Some(100),);
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_timeout) => break,
            }
        }
        assert!(
            found_warn,
            "truncation WARN must be captured with channel attribution",
        );
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

        ch.pending_approvals.lock().await.insert(
            approval_id.clone(),
            PendingApproval {
                sender: tx,
                tool_name: "shell".to_string(),
            },
        );

        // simulate what listen() does when a callback_query arrives
        if let Some(pending) = ch.pending_approvals.lock().await.remove(&approval_id) {
            pending
                .sender
                .send(ChannelApprovalResponse::Approve)
                .unwrap();
        }

        let result = rx.await.unwrap();
        assert_eq!(result, ChannelApprovalResponse::Approve);
    }

    #[tokio::test]
    async fn approval_callback_edits_message_with_outcome_and_drops_keyboard() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/answerCallbackQuery$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/editMessageText$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 99 }
            })))
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

        let approval_id = "abc-123".to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        ch.pending_approvals.lock().await.insert(
            approval_id.clone(),
            PendingApproval {
                sender: tx,
                tool_name: "shell".to_string(),
            },
        );

        let callback = serde_json::json!({
            "id": "cb-1",
            "from": { "first_name": "zeroclaw_operator" },
            "message": { "message_id": 99, "chat": { "id": 12345 } },
            "data": format!("approval:{approval_id}:approve"),
        });

        ch.handle_approval_callback(&callback).await;
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);

        let edit = &requests[1];
        assert!(edit.url.path().ends_with("/editMessageText"));
        let body: serde_json::Value = serde_json::from_slice(&edit.body).unwrap();
        assert_eq!(body["chat_id"], 12345);
        assert_eq!(body["message_id"], 99);
        let approved = i18n::get_required_cli_string("channel-telegram-approval-ack-approved");
        assert_eq!(
            body["text"],
            format!("✅ {approved} by zeroclaw_operator: shell")
        );
        // the keyboard must be removed with a valid InlineKeyboardMarkup
        // (inline_keyboard is a required field); a bare {} payload would be
        // rejected by the Bot API and leave the stale buttons live
        assert_eq!(
            body["reply_markup"],
            serde_json::json!({ "inline_keyboard": [] })
        );
        let rows = body["reply_markup"]["inline_keyboard"].as_array().unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn edit_envelope_ok_false_is_classified_failed() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/edit$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: message to edit not found"
            })))
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

        let resp = ch
            .http_client()
            .post(ch.api_url("edit"))
            .send()
            .await
            .unwrap();
        let classified = TelegramChannel::classify_edit_message_response(resp).await;
        assert_eq!(
            classified,
            EditMessageResult::Failed(reqwest::StatusCode::OK)
        );
    }

    #[tokio::test]
    async fn edit_envelope_ok_false_not_modified_is_classified_not_modified() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/edit$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: message is not modified"
            })))
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

        let resp = ch
            .http_client()
            .post(ch.api_url("edit"))
            .send()
            .await
            .unwrap();
        let classified = TelegramChannel::classify_edit_message_response(resp).await;
        assert_eq!(classified, EditMessageResult::NotModified);
    }

    #[tokio::test]
    async fn edit_envelope_ok_true_is_classified_success() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/edit$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 1 }
            })))
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

        let resp = ch
            .http_client()
            .post(ch.api_url("edit"))
            .send()
            .await
            .unwrap();
        let classified = TelegramChannel::classify_edit_message_response(resp).await;
        assert_eq!(classified, EditMessageResult::Success);
    }

    #[tokio::test]
    async fn edit_envelope_without_ok_field_is_classified_failed() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/edit$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": { "message_id": 1 }
            })))
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

        let resp = ch
            .http_client()
            .post(ch.api_url("edit"))
            .send()
            .await
            .unwrap();
        let classified = TelegramChannel::classify_edit_message_response(resp).await;
        assert_eq!(
            classified,
            EditMessageResult::Failed(reqwest::StatusCode::OK)
        );
    }

    #[tokio::test]
    async fn edit_envelope_malformed_body_is_classified_failed() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/edit$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(b"truncated 200 {\"ok\":", "text/plain"),
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

        let resp = ch
            .http_client()
            .post(ch.api_url("edit"))
            .send()
            .await
            .unwrap();
        let classified = TelegramChannel::classify_edit_message_response(resp).await;
        assert_eq!(
            classified,
            EditMessageResult::Failed(reqwest::StatusCode::OK)
        );
    }

    #[tokio::test]
    async fn process_update_routes_callback_to_single_edit_and_advances_offset() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/answerCallbackQuery$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/editMessageText$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 77 }
            })))
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

        let approval_id = "cb-route-1".to_string();
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        ch.pending_approvals.lock().await.insert(
            approval_id.clone(),
            PendingApproval {
                sender: resp_tx,
                tool_name: "shell".to_string(),
            },
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(4);
        let mut offset = 0i64;
        let mut transient_retry = None;
        let update = serde_json::json!({
            "update_id": 41,
            "callback_query": {
                "id": "cb-route-1",
                "from": { "first_name": "zeroclaw_operator" },
                "message": { "message_id": 77, "chat": { "id": 12345 } },
                "data": format!("approval:{approval_id}:approve"),
            }
        });

        let outcome = ch
            .process_update(&update, &tx, &mut offset, &mut transient_retry)
            .await;
        assert!(matches!(outcome, UpdateOutcome::Advanced));
        assert_eq!(offset, 42);
        assert_eq!(resp_rx.await.unwrap(), ChannelApprovalResponse::Approve);
        assert!(
            rx.try_recv().is_err(),
            "a callback_query is terminal: no downstream message may be delivered"
        );

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.path().ends_with("/answerCallbackQuery"));
        assert!(requests[1].url.path().ends_with("/editMessageText"));
        let edit_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(
            edit_body["reply_markup"],
            serde_json::json!({ "inline_keyboard": [] })
        );
    }

    #[tokio::test]
    async fn timeout_wins_claim_and_late_callback_is_a_stale_no_edit() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/sendMessage$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 55 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/answerCallbackQuery$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        // the runtime already denied on the timeout, so a late tap must not
        // rewrite the card
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/editMessageText$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true
            })))
            .expect(0)
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
        .with_approval_timeout_secs(1);

        let request = zeroclaw_api::channel::ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls -la".to_string(),
            raw_arguments: None,
        };
        let attributed = ch
            .request_approval_attributed("12345", &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            attributed.response,
            zeroclaw_api::channel::ChannelApprovalResponse::Deny
        );

        // the operator's tap arrives after the runtime already denied: the
        // card must stay untouched and the toast must be honest
        let requests = mock_server.received_requests().await.unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let cb_data = sent["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap()
            .to_string();
        let approval_id = cb_data
            .strip_prefix("approval:")
            .unwrap()
            .split(':')
            .next()
            .unwrap()
            .to_string();

        let callback = serde_json::json!({
            "id": "cb-late",
            "from": { "first_name": "zeroclaw_operator" },
            "message": { "message_id": 55, "chat": { "id": 12345 } },
            "data": format!("approval:{approval_id}:approve"),
        });
        ch.handle_approval_callback(&callback).await;

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "no editMessageText may be sent");
        let toast: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        let stale = i18n::get_required_cli_string("channel-telegram-approval-ack-already-resolved");
        assert_eq!(toast["text"], format!("⏳ {stale}"));
    }

    #[tokio::test]
    async fn callback_wins_claim_and_runtime_honors_operator_response() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/sendMessage$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 66 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/answerCallbackQuery$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/editMessageText$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 66 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mention_only = false;
        let ch = Arc::new(
            TelegramChannel::new(
                "fake-token".into(),
                "telegram_test_alias",
                Arc::new(|| vec!["*".into()]),
                mention_only,
            )
            .with_api_base(mock_server.uri())
            .with_approval_timeout_secs(120),
        );

        let request = zeroclaw_api::channel::ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls -la".to_string(),
            raw_arguments: None,
        };
        let waiter = {
            let ch = Arc::clone(&ch);
            zeroclaw_spawn::spawn!(async move {
                ch.request_approval_attributed("12345", &request).await
            })
        };

        let approval_id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(id) = ch.pending_approvals.lock().await.keys().next().cloned() {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("approval entry must be registered");

        let callback = serde_json::json!({
            "id": "cb-early",
            "from": { "first_name": "zeroclaw_operator" },
            "message": { "message_id": 66, "chat": { "id": 12345 } },
            "data": format!("approval:{approval_id}:approve"),
        });
        ch.handle_approval_callback(&callback).await;

        let attributed = waiter.await.unwrap().unwrap().unwrap();
        assert_eq!(
            attributed.response,
            zeroclaw_api::channel::ChannelApprovalResponse::Approve
        );

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[1].url.path().ends_with("/answerCallbackQuery"));
        assert!(requests[2].url.path().ends_with("/editMessageText"));
        let edit_body: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
        let approved = i18n::get_required_cli_string("channel-telegram-approval-ack-approved");
        assert_eq!(
            edit_body["text"],
            format!("✅ {approved} by zeroclaw_operator: shell")
        );
        assert_eq!(
            edit_body["reply_markup"],
            serde_json::json!({ "inline_keyboard": [] })
        );
    }

    #[tokio::test]
    async fn deadline_resolver_loses_claim_and_consumes_callback_response() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        ch.pending_approvals.lock().await.insert(
            "a1".to_string(),
            PendingApproval {
                sender: tx,
                tool_name: "shell".to_string(),
            },
        );
        // the callback claims the entry first and its response is already in
        // flight on the channel when the deadline fires
        let pending = ch.pending_approvals.lock().await.remove("a1").unwrap();
        let _ = pending.sender.send(ChannelApprovalResponse::Approve);

        let outcome = ch.resolve_after_deadline("a1", &mut rx).await;
        assert_eq!(outcome.response, ChannelApprovalResponse::Approve);
        assert!(ch.pending_approvals.lock().await.is_empty());
    }

    #[tokio::test]
    async fn deadline_resolver_wins_claim_and_denies() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let mention_only = false;
        let ch = TelegramChannel::new(
            "fake-token".into(),
            "telegram_test_alias",
            Arc::new(|| vec!["*".into()]),
            mention_only,
        );

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        ch.pending_approvals.lock().await.insert(
            "a2".to_string(),
            PendingApproval {
                sender: tx,
                tool_name: "shell".to_string(),
            },
        );

        let outcome = ch.resolve_after_deadline("a2", &mut rx).await;
        assert_eq!(outcome.response, ChannelApprovalResponse::Deny);
        assert!(
            ch.pending_approvals.lock().await.is_empty(),
            "the winning claim removes the entry"
        );
    }

    #[tokio::test]
    async fn request_approval_localizes_heading_and_keeps_callback_data_exact() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/bot[^/]+/sendMessage$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 1 }
            })))
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
        .with_approval_timeout_secs(1);

        let request = zeroclaw_api::channel::ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls -la".to_string(),
            raw_arguments: None,
        };

        // No one resolves the pending oneshot — the short timeout above lets
        // this return (as a timeout Deny) instead of hanging the test.
        let _ = ch.request_approval("12345", &request).await;

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();

        // Rebuild the expected text via the SAME Fluent keys the
        // implementation uses (locale-agnostic: this holds whatever locale
        // the test process resolves to) — a wiring regression that stops
        // calling i18n, or a typo'd key, changes this and fails the test.
        let heading = i18n::get_required_cli_string("channel-approval-heading");
        let tool_label = i18n::get_required_cli_string("channel-approval-tool-label");
        let tap_instruction = i18n::get_required_cli_string("channel-approval-tap-instruction");
        let expected_text = format!(
            "\u{1f527} <b>{heading}</b>\n\n{tool_label}: <code>shell</code>\nls -la\n\n{tap_instruction}",
        );
        assert_eq!(body["text"], expected_text);

        // Protocol-exact: callback_data must stay `approval:<id>:approve|deny|always`
        // regardless of locale, and all three buttons must share one id.
        let buttons = body["reply_markup"]["inline_keyboard"][0]
            .as_array()
            .unwrap();
        assert_eq!(buttons.len(), 3);
        let mut ids = std::collections::HashSet::new();
        let mut actions = Vec::new();
        for btn in buttons {
            let cb = btn["callback_data"].as_str().unwrap();
            let rest = cb.strip_prefix("approval:").expect("callback_data prefix");
            let (id, action) = rest
                .rsplit_once(':')
                .expect("callback_data has an action suffix");
            ids.insert(id.to_string());
            actions.push(action.to_string());
        }
        assert_eq!(ids.len(), 1, "all three buttons share one approval id");
        assert_eq!(actions, vec!["approve", "deny", "always"]);
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
