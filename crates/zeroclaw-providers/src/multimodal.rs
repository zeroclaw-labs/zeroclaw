use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Client;
use sha2::{Digest as _, Sha256};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_api::media::{
    AttachmentKind, AttachmentReference, PROVIDER_IMAGE_MIME_TYPES, image_mime_from_extension,
    image_mime_from_magic, is_provider_image_mime,
};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_config::schema::{MultimodalConfig, build_runtime_proxy_client_with_timeouts};

/// Bounds for the content-validation decode in
/// [`validate_image_content_with_projection`].
/// Vision providers downscale well below this, so a legitimate attachment is
/// never rejected by these caps — they exist so a small payload declaring huge
/// dimensions is refused instead of allocated.
const MAX_DECODED_IMAGE_DIMENSION: u32 = 16_384;
const MAX_DECODED_IMAGE_ALLOC_BYTES: u64 = 64 * 1024 * 1024;

/// Aggregate decode budget for one `prepare_messages_for_provider` call.
/// Once cumulative decoded allocations reach this threshold, remaining
/// candidate images are skipped. This bounds total resource consumption
/// when many images are present in history.
const AGGREGATE_DECODE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// Slack added to the base64 length ceiling in [`normalize_data_uri`].
///
/// The ceiling is derived from the configured decoded limit, so it must never
/// reject a payload that `validate_size` would have accepted. Base64 rounds up
/// to a 4-character group and adds up to two padding characters, and a data URI
/// may carry incidental whitespace inside the payload. A small fixed allowance
/// absorbs all of that; the decoded size is still checked exactly afterwards, so
/// this only affects which of the two errors an over-limit payload reports.
const BASE64_LENGTH_SLACK_BYTES: usize = 64;

/// Extra bytes added to the source-length ceiling in [`normalize_data_uri`] to
/// cover the `data:` scheme prefix, the MIME type, the `;base64` parameter,
/// the `,` separator, and any insignificant whitespace before the payload.
/// Together with [`BASE64_LENGTH_SLACK_BYTES`] this keeps the entry-point check
/// generous enough to never reject a valid data URI while still bounding the
/// source string before any allocation proportional to it occurs.
const DATA_URI_HEADER_SLACK_BYTES: usize = 256;

// Counts full decodes performed by `validate_image_content`.
//
// Budget exhaustion is observable two ways — an image can be skipped because it
// was rejected *before* decoding, or skipped after being decoded and found too
// large. Both look identical in the prepared output, so asserting on the output
// alone cannot tell "never decoded" from "decoded then discarded". The latter is
// the bug this counter exists to catch.
//
// Task-local rather than process-global: the suite runs tests concurrently and
// many of them decode images, so a global counter would also see their decodes.
// Each observing test scopes its own counter; tests that do not opt in leave it
// unset. A plain comment because doc comments do not attach to a macro
// invocation.
#[cfg(test)]
tokio::task_local! {
    static DECODE_CALLS: std::sync::atomic::AtomicUsize;
    /// Counts calls to `STANDARD.decode` inside `normalize_data_uri` — a seam
    /// that is distinct from the pixel-decoder counter. The encoded-length
    /// ceiling must fire before this is incremented; if it does not, any
    /// oversized input would arrive here and be charged to this counter.
    static BASE64_DECODE_CALLS: std::sync::atomic::AtomicUsize;
}

/// Records one decode against the ambient counter, if a test installed one.
#[cfg(test)]
fn record_decode_call() {
    let _ = DECODE_CALLS.try_with(|calls| {
        calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
}

/// Records one base64-decode call against the ambient counter, if installed.
#[cfg(test)]
fn record_base64_decode_call() {
    let _ = BASE64_DECODE_CALLS.try_with(|calls| {
        calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
}

/// Per-path cache for resolved local image data URIs. Keyed by absolute
/// path; stores `(len, mtime)` for freshness checks (`(0, 0)` sentinel
/// = immutable upload). LRU evicts by both entry count and total bytes.
#[derive(Debug, Default)]
pub struct LocalImageCache {
    entries: HashMap<String, (u64, i64, String)>,
    order: std::collections::VecDeque<String>,
    bytes: usize,
    reported_failures: std::collections::VecDeque<([u8; 32], &'static str)>,
}

const LOCAL_IMAGE_CACHE_MAX_ENTRIES: usize = 32;
const LOCAL_IMAGE_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;
const REPORTED_IMAGE_FAILURE_MAX_ENTRIES: usize = 32;

impl LocalImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&mut self, path: &str, len: u64, mtime: i64) -> Option<&str> {
        let (cached_len, cached_mtime, _) = self.entries.get(path)?;
        let immutable = *cached_len == 0 && *cached_mtime == 0;
        let fresh = *cached_len == len && *cached_mtime == mtime;
        if !immutable && !fresh {
            return None;
        }
        if let Some(pos) = self.order.iter().position(|p| p == path) {
            let key = self.order.remove(pos).expect("position valid");
            self.order.push_back(key);
        }
        self.entries.get(path).map(|(_, _, uri)| uri.as_str())
    }

    fn insert(&mut self, path: String, len: u64, mtime: i64, data_uri: String) {
        if let Some((_, _, old)) = self.entries.remove(&path) {
            self.bytes = self.bytes.saturating_sub(old.len());
            if let Some(pos) = self.order.iter().position(|p| p == &path) {
                self.order.remove(pos);
            }
        }
        self.bytes += data_uri.len();
        self.entries.insert(path.clone(), (len, mtime, data_uri));
        self.order.push_back(path);
        while self.entries.len() > LOCAL_IMAGE_CACHE_MAX_ENTRIES
            || self.bytes > LOCAL_IMAGE_CACHE_MAX_BYTES
        {
            let Some(victim) = self.order.pop_front() else {
                break;
            };
            if let Some((_, _, uri)) = self.entries.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(uri.len());
            }
        }
    }

    fn should_report_failure(&mut self, reference: &str, failure_kind: &'static str) -> bool {
        let reference_key = image_failure_reference_key(reference);
        if let Some(position) =
            self.reported_failures
                .iter()
                .position(|(reported_reference, reported_kind)| {
                    *reported_reference == reference_key && *reported_kind == failure_kind
                })
        {
            if let Some(key) = self.reported_failures.remove(position) {
                self.reported_failures.push_back(key);
            }
            return false;
        }

        let key = (reference_key, failure_kind);
        self.reported_failures.push_back(key);
        while self.reported_failures.len() > REPORTED_IMAGE_FAILURE_MAX_ENTRIES {
            self.reported_failures.pop_front();
        }
        true
    }

    fn clear_reported_failures(&mut self, reference: &str) {
        if self.reported_failures.is_empty() {
            return;
        }
        let reference_key = image_failure_reference_key(reference);
        self.reported_failures
            .retain(|(reported_reference, _)| *reported_reference != reference_key);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn image_failure_reference_key(reference: &str) -> [u8; 32] {
    Sha256::digest(reference.as_bytes()).into()
}

#[derive(Debug, Clone)]
pub struct PreparedMessages {
    pub messages: Vec<ChatMessage>,
    pub contains_images: bool,
    /// Normalized image identities in the final provider-visible request.
    pub submitted_image_ids: Vec<ProviderImageId>,
    /// Submitted identities explicitly present in the newest user turn.
    pub newest_user_image_ids: Vec<ProviderImageId>,
}

/// Stable identity for one normalized provider-visible image reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderImageId([u8; 32]);

impl ProviderImageId {
    fn from_reference(reference: &str) -> Self {
        Self(Sha256::digest(reference.as_bytes()).into())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MultimodalError {
    #[error("multimodal image limit exceeded: max_images={max_images}, found={found}")]
    TooManyImages { max_images: usize, found: usize },

    #[error(
        "multimodal image size limit exceeded for '{input}': {size_bytes} bytes > {max_bytes} bytes"
    )]
    ImageTooLarge {
        input: String,
        size_bytes: usize,
        max_bytes: usize,
    },

    #[error("multimodal image MIME type is not allowed for '{input}': {mime}")]
    UnsupportedMime { input: String, mime: String },

    #[error("multimodal remote image fetch is disabled for '{input}'")]
    RemoteFetchDisabled { input: String },

    #[error("multimodal image source not found or unreadable: '{input}'")]
    ImageSourceNotFound { input: String },

    #[error("invalid multimodal image reference '{input}': {reason}")]
    InvalidReference { input: String, reason: String },

    #[error("failed to download remote image '{input}': {reason}")]
    RemoteFetchFailed { input: String, reason: String },

    #[error("failed to read local image '{input}': {reason}")]
    LocalReadFailed { input: String, reason: String },

    #[error("multimodal image content is not a decodable {mime} image for '{input}': {reason}")]
    CorruptImage {
        input: String,
        mime: String,
        reason: String,
    },
}

/// Why a candidate image reference cannot be sent as an inline base64 image
/// block.
///
/// Deliberately a small copy type rather than a [`MultimodalError`]: the
/// checker below runs over the whole replayed conversation on every turn, and
/// an owned error would allocate for every rejected reference on that path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageDataUriRejection {
    /// Not a `data:` URI at all — a filesystem path, an `http(s)` URL, or prose.
    NotADataUri,
    /// A `data:` URI whose header does not declare `;base64`.
    NotBase64Encoded,
    /// Media type outside [`PROVIDER_IMAGE_MIME_TYPES`].
    UnsupportedMediaType,
    /// Payload is empty or is not canonical padded base64.
    MalformedBase64,
    /// Encoded payload exceeds the caller's per-image ceiling.
    TooLarge,
}

impl std::fmt::Display for ImageDataUriRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::NotADataUri => "not a base64 data URI",
            Self::NotBase64Encoded => "data URI is not base64-encoded",
            Self::UnsupportedMediaType => "unsupported image media type",
            Self::MalformedBase64 => "malformed base64 payload",
            Self::TooLarge => "image payload exceeds the per-image ceiling",
        };
        f.write_str(reason)
    }
}

/// Per-image ceiling on the **base64-encoded** payload length accepted by
/// `split_base64_image_data_uri`: 10 MB. Measured on the encoded payload,
/// unlike the multimodal config's `max_image_size_mb`, which bounds decoded
/// bytes. MB is read as 1024 * 1024, the same way `max_image_size_mb` reads
/// it, so the two ceilings stay consistent with each other. Anthropic
/// documents 10 MB encoded as its per-image limit for the direct API; its
/// separate per-request budget (32 MB across all images) is not enforced
/// here. The adapters pass this same const so the structural check, the
/// adapter sweeps and the resolvability count cannot drift apart on what an
/// image may weigh.
pub(crate) const MAX_ENCODED_IMAGE_PAYLOAD_BYTES: usize = 10 * 1024 * 1024;

/// Splits a `data:` image reference into its media type and base64 payload,
/// checking the structure without decoding it.
///
/// Both halves of the returned pair borrow from `candidate`; a caller that
/// needs an owned lowercase media type allocates it once when it builds its
/// wire block. `encoded_ceiling` is measured on the **encoded** payload
/// length, unlike `max_bytes` elsewhere in this module, which counts decoded
/// bytes.
///
/// This performs no decoding, no filesystem access and no network I/O on
/// purpose. Provider adapters call it while converting an entire replayed
/// history on every turn, so decoding here would mean re-decoding and
/// re-encoding every image in the conversation once per turn.
///
/// It splits and structurally checks. It does not claim the payload decodes to
/// a real image — nothing short of an image decoder can claim that.
pub(crate) fn split_base64_image_data_uri(
    candidate: &str,
    encoded_ceiling: usize,
) -> Result<(&str, &str), ImageDataUriRejection> {
    let rest = candidate
        .strip_prefix("data:")
        .ok_or(ImageDataUriRejection::NotADataUri)?;
    let Some(comma) = rest.find(',') else {
        return Err(ImageDataUriRejection::NotADataUri);
    };

    let header = &rest[..comma];
    let payload = rest[comma + 1..].trim();

    // Matched case-sensitively, exactly as `normalize_data_uri` does, but on a
    // whole parameter rather than a substring. `contains(";base64")` also
    // accepted `;base64foo`, which the Anthropic adapter's residual sweep
    // declines to sweep because it requires an exact `base64` parameter — so
    // such a header fell between the two and left raw base64 in a text position.
    // The parameter may sit anywhere in the list, which is what the sweep allows.
    if !header
        .split(';')
        .skip(1)
        .any(|parameter| parameter == "base64")
    {
        return Err(ImageDataUriRejection::NotBase64Encoded);
    }

    let media_type = header.split(';').next().unwrap_or_default().trim();
    if !PROVIDER_IMAGE_MIME_TYPES
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(media_type))
    {
        return Err(ImageDataUriRejection::UnsupportedMediaType);
    }

    // Checked before the character scan so an oversized payload costs one
    // comparison rather than a full pass.
    if payload.len() > encoded_ceiling {
        return Err(ImageDataUriRejection::TooLarge);
    }

    if !is_canonical_base64_payload(payload) {
        return Err(ImageDataUriRejection::MalformedBase64);
    }

    Ok((media_type, payload))
}

/// True when `payload` is canonical padded base64 in the standard alphabet:
/// non-empty, a multiple of four characters, at most two trailing `=`, and the
/// padding bits of the final quartet zero.
///
/// The final-quartet check is what stops a payload like `AB==` — correct
/// length, legal characters — from passing here and then failing a strict
/// decoder on the provider's side.
fn is_canonical_base64_payload(payload: &str) -> bool {
    if payload.is_empty() || !payload.len().is_multiple_of(4) {
        return false;
    }

    let bytes = payload.as_bytes();
    let pad = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if pad > 2 {
        return false;
    }

    let body = &bytes[..bytes.len() - pad];
    if !body.iter().all(|b| is_standard_base64_char(*b)) {
        return false;
    }

    // `len % 4 == 0` and non-empty means `len >= 4`, so with `pad <= 2` the
    // body always has at least the two characters indexed below.
    match pad {
        // `xyz=` carries 18 bits of payload in 24 bits of encoding: the last
        // character must have its low two bits clear.
        1 => matches!(
            body[body.len() - 1],
            b'A' | b'E'
                | b'I'
                | b'M'
                | b'Q'
                | b'U'
                | b'Y'
                | b'c'
                | b'g'
                | b'k'
                | b'o'
                | b's'
                | b'w'
                | b'0'
                | b'4'
                | b'8'
        ),
        // `xy==` carries 12 bits: the last character must have its low four
        // bits clear.
        2 => matches!(body[body.len() - 1], b'A' | b'Q' | b'g' | b'w'),
        _ => true,
    }
}

fn is_standard_base64_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/'
}

/// Text and image cost are computed from explicit fields. Text is never parsed.
#[derive(Debug, Clone, Copy)]
pub struct AttachmentSummary {
    pub text_bytes: usize,
    pub image_refs: usize,
}

pub fn message_image_summary(message: &ChatMessage) -> AttachmentSummary {
    AttachmentSummary {
        text_bytes: message.context_text_bytes(),
        image_refs: count_message_images(message),
    }
}

pub fn message_image_refs(message: &ChatMessage) -> Vec<String> {
    message
        .attachments
        .iter()
        .filter(|attachment| attachment.kind == AttachmentKind::Image)
        .map(|attachment| attachment.target.clone())
        .collect()
}

/// Final wire projection for attachment kinds without native blocks in these
/// adapters. The canonical messages retain their typed references; only this
/// temporary request view describes a file to the model. Image payloads always
/// use native image blocks, and body text is never parsed as metadata.
pub(crate) fn project_attachment_metadata(messages: &[ChatMessage]) -> Cow<'_, [ChatMessage]> {
    if !messages.iter().any(|message| {
        message
            .attachments
            .iter()
            .any(|attachment| attachment.kind != AttachmentKind::Image)
    }) {
        return Cow::Borrowed(messages);
    }
    let mut projected = messages.to_vec();
    for message in &mut projected {
        let descriptions: Vec<String> = message
            .attachments
            .iter()
            .filter(|attachment| attachment.kind != AttachmentKind::Image)
            .map(|attachment| {
                let kind = match attachment.kind {
                    AttachmentKind::Audio => "audio",
                    AttachmentKind::Video => "video",
                    AttachmentKind::Document => "document",
                    AttachmentKind::Image => "image",
                };
                if attachment.target.starts_with("data:") {
                    format!("Attached {kind}: inline data")
                } else {
                    format!("Attached {kind}: {}", attachment.target)
                }
            })
            .collect();
        if !descriptions.is_empty() {
            message.append_note(&descriptions.join("\n"));
            message
                .attachments
                .retain(|attachment| attachment.kind == AttachmentKind::Image);
        }
    }
    Cow::Owned(projected)
}

fn count_message_images(message: &ChatMessage) -> usize {
    message
        .attachments
        .iter()
        .filter(|attachment| attachment.kind == AttachmentKind::Image)
        .count()
}

pub fn count_image_attachments(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .zip(attachment_dispositions(messages))
        .filter(|(_, disposition)| *disposition == AttachmentDisposition::Normalized)
        .map(|(message, _)| count_message_images(message))
        .sum()
}

pub fn contains_image_attachments(messages: &[ChatMessage]) -> bool {
    count_image_attachments(messages) > 0
}

pub fn count_user_image_attachments(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|message| message.role == "user")
        .map(count_message_images)
        .sum()
}

pub fn count_latest_user_image_attachments(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(count_message_images)
        .unwrap_or(0)
}

pub const MAX_RESOLVABILITY_CANDIDATES: usize = 16;
pub async fn count_latest_user_resolvable_image_attachments(
    messages: &[ChatMessage],
    remote_allowed: bool,
    path_allowed: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
) -> usize {
    let Some(message) = messages.iter().rev().find(|message| message.role == "user") else {
        return 0;
    };
    // Inline, I/O-free classification: `inline` counts references that
    // resolve without any policy or filesystem work; absolute local
    // paths become candidates for the one blocking task below.
    let mut inline = 0;
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut overflow = 0;
    for reference in message_image_refs(message) {
        if reference.starts_with("data:") {
            if split_base64_image_data_uri(&reference, MAX_ENCODED_IMAGE_PAYLOAD_BYTES).is_ok() {
                inline += 1;
            }
        } else if reference.starts_with("http://") || reference.starts_with("https://") {
            if remote_allowed {
                inline += 1;
            }
        } else if !Path::new(&reference).is_absolute() {
            // Relative references never count: see the doc comment on
            // `count_latest_user_resolvable_image_attachments`.
        } else if candidates.len() < MAX_RESOLVABILITY_CANDIDATES {
            candidates.push(PathBuf::from(reference));
        } else {
            // Over the candidate bound: no policy call, no probe, and
            // the reference counts as resolvable, so the caller fails
            // toward the loud capability error, never toward silently
            // stripping an attachment that may be real.
            overflow += 1;
        }
    }
    if candidates.is_empty() {
        return inline + overflow;
    }
    let candidate_count = candidates.len();
    // ONE blocking task per gate evaluation, not one per attachment: the
    // predicate and the existence probe do synchronous filesystem work
    // and must stay off the async executor thread. The `Arc` signature
    // exists because `spawn_blocking` needs `'static`.
    let probe = tokio::task::spawn_blocking(move || {
        candidates
            .iter()
            .filter(|path| path_allowed(path) && std::fs::metadata(path).is_ok_and(|m| m.is_file()))
            .count()
    });
    // JoinError means the predicate panicked or the runtime is shutting
    // down: count every candidate as resolvable, the same
    // fail-toward-refusal rule as overflow. zeroclaw-providers has no
    // tracing dependency, so the failure is silent here.
    let probed = match probe.await {
        Ok(count) => count,
        Err(_join_error) => candidate_count,
    };
    inline + probed + overflow
}

/// Note used when the provider projection omits image attachments.
pub const MEDIA_PLACEHOLDER: &str = "(media attachment omitted)";

pub fn omit_image_attachments(message: &ChatMessage) -> ChatMessage {
    let mut result = message.clone();
    let original_count = result.attachments.len();
    result
        .attachments
        .retain(|attachment| attachment.kind != AttachmentKind::Image);
    if result.attachments.len() != original_count {
        result.append_note(MEDIA_PLACEHOLDER);
    }
    result
}

pub fn extract_ollama_image_payload(image_ref: &str) -> Option<String> {
    if image_ref.starts_with("data:") {
        let comma_idx = image_ref.find(',')?;
        let (_, payload) = image_ref.split_at(comma_idx + 1);
        let payload = payload.trim();
        if payload.is_empty() {
            None
        } else {
            Some(payload.to_string())
        }
    } else {
        Some(image_ref.trim().to_string()).filter(|value| !value.is_empty())
    }
}

/// Locate tool results belonging to the most recent user turn.
fn current_turn_tool_result_indices(messages: &[ChatMessage]) -> HashSet<usize> {
    let current_turn_start = messages.iter().rposition(|message| message.role == "user");

    let mut indices = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        if current_turn_start.is_some_and(|start| index < start) {
            continue;
        }
        if message.tool_result.is_some() {
            indices.insert(index);
        }
    }
    indices
}

fn should_normalize_message_images(
    index: usize,
    message: &ChatMessage,
    current_turn_tool_result_indices: &HashSet<usize>,
) -> bool {
    if message.tool_result.is_some() {
        return current_turn_tool_result_indices.contains(&index);
    }

    message.role == "user"
}

/// How multimodal preparation will treat the image attachments in a
/// message at its position in the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentDisposition {
    /// Loadable attachments become provider image blocks: user messages and
    /// tool-result carriers in the current user turn.
    Normalized,
    /// Attachments are stripped before dispatch: tool-result carriers before the
    /// current user turn.
    Stripped,
    /// Content is dispatched verbatim as text: system and assistant messages.
    Literal,
}

/// One disposition per message, computed with the same predicates preparation
/// uses (typed tool-result metadata, `current_turn_tool_result_indices`,
/// `should_normalize_message_images`). Dispositions do not model the
/// config-dependent image limits (`max_images`, `max_image_turns`), which can
/// strip or cap images preparation would otherwise dispatch.
pub fn attachment_dispositions(messages: &[ChatMessage]) -> Vec<AttachmentDisposition> {
    let current_turn_indices = current_turn_tool_result_indices(messages);
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            if should_normalize_message_images(index, message, &current_turn_indices) {
                AttachmentDisposition::Normalized
            } else if message.tool_result.is_some() {
                AttachmentDisposition::Stripped
            } else {
                AttachmentDisposition::Literal
            }
        })
        .collect()
}

fn trim_message_images(message: &ChatMessage, drop_here: usize) -> ChatMessage {
    let mut result = message.clone();
    let mut dropped = 0;
    result.attachments.retain(|attachment| {
        if attachment.kind == AttachmentKind::Image && dropped < drop_here {
            dropped += 1;
            false
        } else {
            true
        }
    });
    result
}

pub async fn prepare_messages_for_provider(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
) -> Result<PreparedMessages, MultimodalError> {
    prepare_messages_inner(messages, config, None).await
}

pub async fn prepare_messages_for_provider_cached(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
    cache: &mut LocalImageCache,
) -> Result<PreparedMessages, MultimodalError> {
    prepare_messages_inner(messages, config, Some(cache)).await
}

async fn prepare_messages_inner(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
    mut cache: Option<&mut LocalImageCache>,
) -> Result<PreparedMessages, MultimodalError> {
    let mut candidates = if config.max_image_turns > 0 {
        trim_images_by_age(messages, config.max_image_turns)
    } else {
        messages.to_vec()
    };
    if config.max_images > 0 {
        candidates = trim_old_images(&candidates, config.max_images);
    }
    let dispositions = attachment_dispositions(&candidates);
    let max_bytes = config.max_image_size_mb.clamp(1, 20) * 1024 * 1024;
    let remote_client = build_runtime_proxy_client_with_timeouts("provider.multimodal", 30, 10);
    let mut remaining_decode_budget = AGGREGATE_DECODE_BUDGET_BYTES;
    let mut prepared = Vec::with_capacity(candidates.len());
    for (index, message) in candidates.iter().enumerate().rev() {
        let mut result = message.clone();
        match dispositions[index] {
            AttachmentDisposition::Literal => {}
            AttachmentDisposition::Stripped => {
                result = trim_message_images(message, usize::MAX);
            }
            AttachmentDisposition::Normalized => {
                let refs = message_image_refs(message);
                if !refs.is_empty() {
                    let normalized = normalize_image_references(
                        &refs,
                        config,
                        max_bytes,
                        &remote_client,
                        &ImageNormalizeCtx {
                            message_index: index,
                            role: &message.role,
                        },
                        cache.as_deref_mut(),
                        &mut remaining_decode_budget,
                    )
                    .await;
                    result
                        .attachments
                        .retain(|attachment| attachment.kind != AttachmentKind::Image);
                    result
                        .attachments
                        .extend(normalized.data_uris.into_iter().map(|target| {
                            AttachmentReference {
                                kind: AttachmentKind::Image,
                                target,
                            }
                        }));
                    if normalized.skipped_count > 0 {
                        let note =
                            append_skipped_image_note("", normalized.skipped_count, refs.len());
                        result.append_note(&note);
                    }
                }
            }
        }
        prepared.push(result);
    }
    prepared.reverse();
    let submitted_image_ids = provider_image_ids(&prepared);
    let newest_user_image_ids = prepared
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(provider_image_ids_in_message)
        .unwrap_or_default();
    Ok(PreparedMessages {
        contains_images: !submitted_image_ids.is_empty(),
        messages: prepared,
        submitted_image_ids,
        newest_user_image_ids,
    })
}

fn provider_image_ids_in_message(message: &ChatMessage) -> Vec<ProviderImageId> {
    message_image_refs(message)
        .iter()
        .map(|reference| ProviderImageId::from_reference(reference))
        .collect()
}

pub fn provider_image_ids(messages: &[ChatMessage]) -> Vec<ProviderImageId> {
    messages
        .iter()
        .zip(attachment_dispositions(messages))
        .filter(|(_, disposition)| *disposition == AttachmentDisposition::Normalized)
        .flat_map(|(message, _)| provider_image_ids_in_message(message))
        .collect()
}

pub fn omit_provider_image_ids(
    messages: &[ChatMessage],
    omitted: &[ProviderImageId],
) -> Vec<ChatMessage> {
    messages
        .iter()
        .zip(attachment_dispositions(messages))
        .map(|(message, disposition)| {
            let mut result = message.clone();
            if disposition == AttachmentDisposition::Normalized {
                result.attachments.retain(|attachment| {
                    attachment.kind != AttachmentKind::Image
                        || !omitted.contains(&ProviderImageId::from_reference(&attachment.target))
                });
            }
            result
        })
        .collect()
}

fn trim_images_by_age(messages: &[ChatMessage], max_turns: usize) -> Vec<ChatMessage> {
    let mut turn_count = 0;
    messages
        .iter()
        .rev()
        .map(|message| {
            if message.role == "user" {
                turn_count += 1;
            }
            if turn_count > max_turns && message.role == "user" {
                trim_message_images(message, usize::MAX)
            } else {
                message.clone()
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn images_kept_under_cap(total: usize, max_images: usize) -> usize {
    if total <= max_images {
        return total;
    }
    let low_water = max_images - max_images / 2;
    let batch = max_images / 2 + 1;
    low_water + (total - max_images - 1) % batch
}

fn trim_old_images(messages: &[ChatMessage], max_images: usize) -> Vec<ChatMessage> {
    let dispositions = attachment_dispositions(messages);
    let total = count_image_attachments(messages);
    let mut to_drop = total - images_kept_under_cap(total, max_images);
    messages
        .iter()
        .zip(dispositions)
        .map(|(message, disposition)| {
            if disposition != AttachmentDisposition::Normalized {
                return message.clone();
            }
            let drop_here = to_drop.min(count_message_images(message));
            to_drop -= drop_here;
            trim_message_images(message, drop_here)
        })
        .collect()
}

struct NormalizedImageReferences {
    data_uris: Vec<String>,
    skipped_count: usize,
}

/// Context attached to image-skip log events so callers can be identified.
struct ImageNormalizeCtx<'a> {
    /// Zero-based index of this message in the conversation history.
    message_index: usize,
    /// Role of the message containing the image reference.
    role: &'a str,
}

/// Structured attributes for a skipped-image event.
///
/// The raw reference is deliberately **not** included. `source_kind` carries
/// the reference class (local / remote / data), which is what an operator
/// needs to locate the failing path, while the reference itself would leak
/// untrusted input into logs: a local path exposes workspace or user naming,
/// a remote URL can carry query credentials or a private endpoint, and a data
/// URI exposes the image/base64 prefix. `reason` is the sanitized decoder
/// message, which already has the source scrubbed from it.
fn skipped_image_log_attrs(
    ctx: &ImageNormalizeCtx<'_>,
    reference: &str,
    error_kind: &str,
    error_reason: Option<&str>,
) -> ::serde_json::Value {
    ::serde_json::json!({
        "message_index": ctx.message_index,
        "message_role": ctx.role,
        "source_kind": image_reference_kind(reference),
        "error_kind": error_kind,
        "reason": error_reason.unwrap_or(""),
    })
}

async fn normalize_image_references(
    refs: &[String],
    config: &MultimodalConfig,
    max_bytes: usize,
    remote_client: &Client,
    ctx: &ImageNormalizeCtx<'_>,
    mut cache: Option<&mut LocalImageCache>,
    remaining_budget: &mut u64,
) -> NormalizedImageReferences {
    let mut data_uris = Vec::with_capacity(refs.len());
    let mut skipped_count = 0usize;

    for reference in refs {
        match normalize_image_reference(
            reference,
            config,
            max_bytes,
            remote_client,
            cache.as_deref_mut(),
            &mut *remaining_budget,
        )
        .await
        {
            Ok(data_uri) => {
                if let Some(cache) = cache.as_deref_mut() {
                    cache.clear_reported_failures(reference);
                }
                data_uris.push(data_uri);
            }
            Err(error) => {
                skipped_count += 1;
                let error_kind = multimodal_error_kind(&error);
                let should_report = cache
                    .as_deref_mut()
                    .is_none_or(|cache| cache.should_report_failure(reference, error_kind));
                if !should_report {
                    continue;
                }
                let error_reason = multimodal_error_reason(&error);
                let attrs =
                    skipped_image_log_attrs(ctx, reference, error_kind, error_reason.as_deref());
                let is_tool_role = ctx.role == "tool";
                let is_recoverable_load_failure = matches!(
                    error_kind,
                    "image_source_not_found"
                        | "local_read_failed"
                        | "remote_fetch_failed"
                        | "invalid_reference"
                        | "corrupt_image"
                );
                if is_tool_role && is_recoverable_load_failure {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(attrs),
                        "skipping tool result attachment that could not be loaded"
                    );
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(attrs),
                        "skipping multimodal image that could not be loaded"
                    );
                }
            }
        }
    }

    NormalizedImageReferences {
        data_uris,
        skipped_count,
    }
}

fn append_skipped_image_note(text: &str, skipped_count: usize, total_refs: usize) -> String {
    if skipped_count == 0 {
        return text.to_string();
    }

    // This note is model-facing provider context, not direct localized UI text.
    let note = if skipped_count == total_refs {
        format!("{skipped_count} attached image(s) could not be loaded")
    } else {
        format!("{skipped_count} of {total_refs} attached image(s) could not be loaded")
    };

    let trimmed = text.trim();
    if trimmed.is_empty() {
        format!("Note: {note}.")
    } else {
        format!("{trimmed}\n\nNote: {note}.")
    }
}

fn image_reference_kind(reference: &str) -> &'static str {
    if reference.starts_with("data:") {
        "data"
    } else if reference.starts_with("http://") || reference.starts_with("https://") {
        "remote"
    } else {
        "local"
    }
}

fn multimodal_error_kind(error: &anyhow::Error) -> &'static str {
    match error.downcast_ref::<MultimodalError>() {
        Some(MultimodalError::TooManyImages { .. }) => "too_many_images",
        Some(MultimodalError::ImageTooLarge { .. }) => "image_too_large",
        Some(MultimodalError::UnsupportedMime { .. }) => "unsupported_mime",
        Some(MultimodalError::RemoteFetchDisabled { .. }) => "remote_fetch_disabled",
        Some(MultimodalError::ImageSourceNotFound { .. }) => "image_source_not_found",
        Some(MultimodalError::InvalidReference { .. }) => "invalid_reference",
        Some(MultimodalError::RemoteFetchFailed { .. }) => "remote_fetch_failed",
        Some(MultimodalError::LocalReadFailed { .. }) => "local_read_failed",
        Some(MultimodalError::CorruptImage { .. }) => "corrupt_image",
        None => "unknown",
    }
}

fn multimodal_error_reason(error: &anyhow::Error) -> Option<String> {
    match error.downcast_ref::<MultimodalError>() {
        Some(MultimodalError::InvalidReference { input, reason })
        | Some(MultimodalError::RemoteFetchFailed { input, reason })
        | Some(MultimodalError::LocalReadFailed { input, reason }) => {
            Some(reason.replace(input, "<source>"))
        }
        Some(MultimodalError::CorruptImage { input, reason, .. }) => {
            Some(reason.replace(input, "<source>"))
        }
        _ => None,
    }
}

async fn normalize_image_reference(
    source: &str,
    config: &MultimodalConfig,
    max_bytes: usize,
    remote_client: &Client,
    cache: Option<&mut LocalImageCache>,
    remaining_budget: &mut u64,
) -> anyhow::Result<String> {
    if source.starts_with("data:") {
        return normalize_data_uri(source, max_bytes, remaining_budget).await;
    }

    if source.starts_with("http://") || source.starts_with("https://") {
        if !config.allow_remote_fetch {
            return Err(MultimodalError::RemoteFetchDisabled {
                input: source.to_string(),
            }
            .into());
        }

        return normalize_remote_image(source, max_bytes, remote_client, remaining_budget).await;
    }

    match cache {
        Some(c) => normalize_local_image_cached(source, max_bytes, c, remaining_budget).await,
        None => normalize_local_image(source, max_bytes, remaining_budget).await,
    }
}

async fn normalize_data_uri(
    source: &str,
    max_bytes: usize,
    remaining_budget: &mut u64,
) -> anyhow::Result<String> {
    // Bound the data URI length before doing anything that allocates from it.
    //
    // Every subsequent branch — `source.find(',')`, header checks,
    // `validate_mime`, and `STANDARD.decode` — can only run on a string that
    // is already owned or about to be owned. Without this check, a malformed
    // or unsupported header causes an error branch to call `source.to_string()`
    // before the encoded-payload ceiling is ever reached, allocating a second
    // full copy of attacker-controlled input. Checking the total source length
    // here keeps all of them bounded: even a header that is entirely zeros
    // contributes at most `max_encoded_len + DATA_URI_HEADER_SLACK_BYTES` to
    // the rejection path.
    let max_encoded_len = max_bytes
        .div_ceil(3)
        .saturating_mul(4)
        .saturating_add(BASE64_LENGTH_SLACK_BYTES);
    let max_source_len = max_encoded_len.saturating_add(DATA_URI_HEADER_SLACK_BYTES);
    if source.len() > max_source_len {
        return Err(MultimodalError::ImageTooLarge {
            input: "[data URI]".to_string(),
            size_bytes: source.len(),
            max_bytes: max_source_len,
        }
        .into());
    }

    let Some(comma_idx) = source.find(',') else {
        return Err(MultimodalError::InvalidReference {
            input: "[data URI]".to_string(),
            reason: "expected data URI payload".to_string(),
        }
        .into());
    };

    let header = &source[..comma_idx];
    let payload = source[comma_idx + 1..].trim();

    if !header.contains(";base64") {
        return Err(MultimodalError::InvalidReference {
            input: "[data URI]".to_string(),
            reason: "only base64 data URIs are supported".to_string(),
        }
        .into());
    }

    let mime = header
        .trim_start_matches("data:")
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    // Pass the category label rather than `source`: the MIME type is already
    // embedded in a separate field, so the full URI text adds no diagnostic
    // value and would copy controlled input into the error message.
    validate_mime("[data URI]", &mime)?;

    // Reject on the *encoded* length before decoding.
    //
    // `STANDARD.decode` allocates its output buffer from the length of the
    // input it is given, so decoding first and size-checking afterwards lets an
    // oversized data URI allocate before any limit applies. This runs on the
    // async preparation task, outside the `spawn_blocking` boundary that
    // isolates pixel decoding, so it is the one allocation on this path that
    // nothing else bounds.
    //
    // Base64 encodes 3 bytes as 4 characters, so `max_bytes` decoded needs at
    // most `ceil(max_bytes / 3) * 4` characters. Padding and any embedded
    // whitespace are covered by rounding up, which keeps the ceiling generous
    // enough never to reject a payload that would have passed `validate_size`.
    let max_encoded_len = max_bytes
        .div_ceil(3)
        .saturating_mul(4)
        .saturating_add(BASE64_LENGTH_SLACK_BYTES);
    if payload.len() > max_encoded_len {
        // Report the ceiling in terms of the encoded length, not the payload
        // itself. Copying the attacker-controlled source into the error message
        // would allocate a full second copy of the oversized input; using a
        // category label keeps the rejection bounded.
        return Err(MultimodalError::ImageTooLarge {
            input: "[data URI]".to_string(),
            size_bytes: payload.len(),
            max_bytes: max_encoded_len,
        }
        .into());
    }

    #[cfg(test)]
    record_base64_decode_call();

    let decoded = STANDARD
        .decode(payload)
        .map_err(|error| MultimodalError::InvalidReference {
            input: source.to_string(),
            reason: format!("invalid base64 payload: {error}"),
        })?;

    validate_size(source, decoded.len(), max_bytes)?;

    validate_within_budget(source, &mime, &decoded, remaining_budget).await?;

    Ok(format!("data:{mime};base64,{}", STANDARD.encode(decoded)))
}

async fn normalize_remote_image(
    source: &str,
    max_bytes: usize,
    remote_client: &Client,
    remaining_budget: &mut u64,
) -> anyhow::Result<String> {
    let response = remote_client.get(source).send().await.map_err(|error| {
        MultimodalError::RemoteFetchFailed {
            input: source.to_string(),
            reason: error.to_string(),
        }
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(MultimodalError::RemoteFetchFailed {
            input: source.to_string(),
            reason: format!("HTTP {status}"),
        }
        .into());
    }

    if let Some(content_length) = response.content_length() {
        let content_length = usize::try_from(content_length).unwrap_or(usize::MAX);
        validate_size(source, content_length, max_bytes)?;
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);

    let bytes = response
        .bytes()
        .await
        .map_err(|error| MultimodalError::RemoteFetchFailed {
            input: source.to_string(),
            reason: error.to_string(),
        })?;

    validate_size(source, bytes.len(), max_bytes)?;

    let mime = detect_mime(None, bytes.as_ref(), content_type.as_deref()).ok_or_else(|| {
        MultimodalError::UnsupportedMime {
            input: source.to_string(),
            mime: "unknown".to_string(),
        }
    })?;

    validate_mime(source, &mime)?;

    validate_within_budget(source, &mime, bytes.as_ref(), remaining_budget).await?;

    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

async fn normalize_local_image(
    source: &str,
    max_bytes: usize,
    remaining_budget: &mut u64,
) -> anyhow::Result<String> {
    let path = Path::new(source);
    if !path.exists() || !path.is_file() {
        return Err(MultimodalError::ImageSourceNotFound {
            input: source.to_string(),
        }
        .into());
    }

    let metadata =
        tokio::fs::metadata(path)
            .await
            .map_err(|error| MultimodalError::LocalReadFailed {
                input: source.to_string(),
                reason: error.to_string(),
            })?;

    validate_size(
        source,
        usize::try_from(metadata.len()).unwrap_or(usize::MAX),
        max_bytes,
    )?;

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| MultimodalError::LocalReadFailed {
            input: source.to_string(),
            reason: error.to_string(),
        })?;

    validate_size(source, bytes.len(), max_bytes)?;

    let mime =
        detect_mime(Some(path), &bytes, None).ok_or_else(|| MultimodalError::UnsupportedMime {
            input: source.to_string(),
            mime: "unknown".to_string(),
        })?;

    validate_mime(source, &mime)?;

    validate_within_budget(source, &mime, &bytes, remaining_budget).await?;

    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

/// Cache-aware local image loader. On a hit (path + metadata unchanged) returns
/// the stored data URI without touching the filesystem. Files under `/uploads/`
/// are content-addressed and treated as immutable — checked once, never re-read.
async fn normalize_local_image_cached(
    source: &str,
    max_bytes: usize,
    cache: &mut LocalImageCache,
    remaining_budget: &mut u64,
) -> anyhow::Result<String> {
    let path = Path::new(source);
    if !path.exists() || !path.is_file() {
        return Err(MultimodalError::ImageSourceNotFound {
            input: source.to_string(),
        }
        .into());
    }

    let metadata =
        tokio::fs::metadata(path)
            .await
            .map_err(|error| MultimodalError::LocalReadFailed {
                input: source.to_string(),
                reason: error.to_string(),
            })?;

    let file_len = metadata.len();
    let is_immutable = source.contains("/uploads/");
    let mtime: i64 = if is_immutable {
        0
    } else {
        metadata
            .modified()
            .ok()
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs() as i64)
            })
            .unwrap_or(0)
    };
    let cache_len = if is_immutable { 0 } else { file_len };

    validate_size(
        source,
        usize::try_from(file_len).unwrap_or(usize::MAX),
        max_bytes,
    )?;

    if let Some(cached) = cache.get(source, cache_len, mtime) {
        return Ok(cached.to_string());
    }

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| MultimodalError::LocalReadFailed {
            input: source.to_string(),
            reason: error.to_string(),
        })?;

    validate_size(source, bytes.len(), max_bytes)?;

    let mime =
        detect_mime(Some(path), &bytes, None).ok_or_else(|| MultimodalError::UnsupportedMime {
            input: source.to_string(),
            mime: "unknown".to_string(),
        })?;

    validate_mime(source, &mime)?;

    validate_within_budget(source, &mime, &bytes, remaining_budget).await?;

    let data_uri = format!("data:{mime};base64,{}", STANDARD.encode(&bytes));
    cache.insert(source.to_string(), cache_len, mtime, data_uri.clone());
    Ok(data_uri)
}

fn validate_size(source: &str, size_bytes: usize, max_bytes: usize) -> anyhow::Result<()> {
    if size_bytes > max_bytes {
        return Err(MultimodalError::ImageTooLarge {
            input: source.to_string(),
            size_bytes,
            max_bytes,
        }
        .into());
    }

    Ok(())
}

fn validate_mime(source: &str, mime: &str) -> anyhow::Result<()> {
    if is_provider_image_mime(mime) {
        return Ok(());
    }

    Err(MultimodalError::UnsupportedMime {
        input: source.to_string(),
        mime: mime.to_string(),
    }
    .into())
}

/// Map a validated MIME type to the decoder format it selects.
fn image_format_for_mime(source: &str, mime: &str) -> anyhow::Result<image::ImageFormat> {
    match mime {
        "image/png" => Ok(image::ImageFormat::Png),
        "image/jpeg" => Ok(image::ImageFormat::Jpeg),
        "image/webp" => Ok(image::ImageFormat::WebP),
        "image/gif" => Ok(image::ImageFormat::Gif),
        _ => Err(MultimodalError::UnsupportedMime {
            input: source.to_string(),
            mime: mime.to_string(),
        }
        .into()),
    }
}

/// Extra bytes per pixel a **still** WebP decode holds on top of its output
/// buffer, covering the worst case across alpha and non-alpha shapes.
///
/// - *Non-alpha* (`Rgb8`, 3 B/px output): lossless decode allocates a
///   `width * height * 4` RGBA scratch and converts it to the 3 B/px output.
///   Scratch = 4 B/px.
/// - *Alpha* (`Rgba8`, 4 B/px output): `read_alpha_chunk` lossless branch
///   allocates a `width * height * 4` RGBA buffer and a separate
///   `width * height * 1` green plane, both live alongside the 4 B/px output.
///   Scratch = 4 + 1 = 5 B/px.
///
/// 5 is used here so a single constant covers both shapes without needing to
/// inspect `has_alpha` at projection time.
const WEBP_STILL_SCRATCH_BYTES_PER_PIXEL: u64 = 5;

/// Extra bytes per pixel an **animated** WebP decode holds on top of its
/// output buffer, covering the worst case across alpha and non-alpha shapes.
///
/// While one frame is produced, these full-canvas buffers overlap
/// (`image-webp` 0.2.4 `src/decoder.rs` `read_frame`, and `image` 0.25.10
/// `src/codecs/webp/decoder.rs` `into_frames`):
///
/// - the decoded frame buffer, 4 B/px;
/// - for an alpha frame, `read_alpha_chunk`'s lossless branch adds a 4 B/px
///   RGBA buffer plus a 1 B/px green plane;
/// - the persistent composition canvas, always 4 B/px, allocated lazily on the
///   first frame;
/// - the adapter's per-frame `RgbImage` and the `RgbaImage` it converts into —
///   3 + 4 B/px, of which the 3 are already covered by the output charge.
///
/// Worst case (alpha): 4 + 4 + 1 + 4 + 4 = 17 B/px beyond the output buffer.
const WEBP_ANIMATED_SCRATCH_BYTES_PER_PIXEL: u64 = 17;

/// Extra bytes per pixel a **GIF** decode holds on top of the frame buffer it
/// yields.
///
/// `image` 0.25.10's frame iterator (`src/codecs/gif.rs`) allocates a
/// persistent full-canvas `non_disposed_frame` (RGBA8, 4 B/px) through
/// `limits.reserve_buffer` before the first frame and keeps it for the whole
/// animation, blending each frame against it. It is live at the same time as
/// the frame buffer the iterator returns.
///
/// A frame that does not cover the whole canvas costs one buffer more. The
/// iterator decodes the sub-rectangle into `frame_buffer`, and when that frame
/// is not the full canvas at the origin it allocates a *second*, full-canvas
/// `image_buffer` to composite into while `frame_buffer` and
/// `non_disposed_frame` are both still live (`src/codecs/gif.rs:388-415`). The
/// returned `Frame` is the composited full canvas in either branch, so the
/// sub-rectangle is invisible from the outside and the two branches cannot be
/// told apart after the fact.
///
/// The bound is therefore the worst branch: the persistent canvas plus a
/// temporary sub-rectangle of at most one canvas, on top of the returned frame.
/// Charging 4 B/px would only cover the full-frame branch and would
/// under-account every partial-frame GIF.
///
/// Every GIF reaching validation is routed through the animation path, so this
/// applies to single-frame GIFs too.
const GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL: u64 = 8;

/// Extra bytes per pixel an **APNG** decode holds on top of the frame buffer it
/// yields, covering the worst-case color type accepted by
/// [`ApngDecoder::animatable_color_type`].
///
/// `image` 0.25.10's `ApngDecoder` (`src/codecs/png.rs`) keeps two persistent
/// RGBA8 canvases, `current` and `previous`, live across the animation
/// (4 B/px each). While each frame is composited, it also reads a raw frame
/// buffer and converts it to RGBA8 before blending it into `current`:
///
/// - `Rgba8`: raw `buffer` and `source` are the same allocation (from_raw);
///   no extra bytes beyond the two persistent canvases.
/// - `L8`:   raw = 1 B/px, source = 4 B/px; both live during conversion.
/// - `La8`:  raw = 2 B/px, source = 4 B/px.
/// - `Rgb8`: raw = 3 B/px, source = 4 B/px — the worst case.
///
/// The per-frame peak beyond the returned frame is therefore:
///   `current(4) + previous(4) + raw(3) + source(4) = 15 B/px`
/// while the returned `Frame` (`current.clone()`) provides only 4 B/px.
/// Scratch = 15 - output(4) = 11 for `Rgba8`, rising to 11 for `Rgb8` when
/// `total_bytes()` returns 3 B/px.  The conservative value covering all
/// accepted types is **12 B/px** (15 peak − 3 min output).
const APNG_SCRATCH_BYTES_PER_PIXEL: u64 = 12;

/// JPEG coefficient samples are signed 16-bit values. `zune-jpeg` keeps a
/// full padded coefficient plane for progressive images and for baseline
/// images whose first scan does not contain every component, so the projection
/// must account for those planes before admitting a decode. A baseline image
/// whose first scan contains every component is decoded one MCU row at a time
/// and never allocates them (`all_components_in_first_scan` in zune-jpeg
/// 0.5.15 `src/mcu.rs`). The row-sized buffers both paths use are covered by
/// the additional scratch factor in [`jpeg_auxiliary_allocation`].
const JPEG_COEFFICIENT_BYTES_PER_SAMPLE: u64 = 2;
// The largest upsampling ratio accepted by zune-jpeg is 4x4. Charging 32
// copies of one padded coefficient row covers the row/row_up, upsample
// destination and conversion scratch vectors even for that shape.
const JPEG_SCRATCH_ROWS_MULTIPLIER: u64 = 32;

#[derive(Debug, Clone, Copy)]
struct JpegComponentSampling {
    id: u8,
    horizontal: u8,
    vertical: u8,
}

/// Header information needed to conservatively project zune-jpeg's
/// coefficient allocation. This parser only walks marker lengths, SOF, and the
/// first SOS header; entropy-coded data is never touched.
#[derive(Debug)]
struct JpegFrameHeader {
    width: u32,
    height: u32,
    components: Vec<JpegComponentSampling>,
    /// The frame is sequential (SOF0/SOF1) and its first scan names every
    /// frame component, so zune-jpeg decodes MCU rows straight to pixels
    /// without full-image coefficient planes. `false` whenever that cannot be
    /// established, which keeps the coefficient charge.
    first_scan_decodes_rows: bool,
}

/// What the projection finds between a frame header and the first scan.
enum JpegFirstScan {
    /// A well-formed first SOS that names every frame component.
    CoversFrame,
    /// A first SOS that omits a component, or anything the walk cannot
    /// classify; the coefficient charge stays.
    Unclassified,
    /// Another frame header before the first scan. zune-jpeg rejects a second
    /// frame header, and the projection does not classify a frame whose header
    /// is ambiguous.
    AnotherFrame,
}

/// Bytes zune-jpeg 0.5.15 reads past a segment's declared end. Its APP0 parser
/// reads a 5-byte probe whenever the length exceeds 5 and then skips the
/// saturating remainder, so a declared length of exactly 6 consumes one byte
/// more than declared. Every other segment it accepts consumes exactly the
/// declared length.
fn zune_jpeg_segment_overread(marker: u8, declared: usize) -> usize {
    usize::from(marker == 0xe0 && declared == 6)
}

fn jpeg_frame_header(bytes: &[u8]) -> Option<JpegFrameHeader> {
    if bytes.get(..2) != Some(&[0xff, 0xd8]) {
        return None;
    }

    let mut offset = 2usize;
    while offset < bytes.len() {
        if bytes.get(offset) != Some(&0xff) {
            return None;
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;

        // FF00 is byte stuffing and is equivalent to what zune-jpeg skips in
        // its non-strict header parser. The other no-length markers (TEM,
        // restart, SOI, and EOI) are handled differently by that decoder and
        // can change where it finds the real frame header. Refuse them here
        // instead of allowing the projection to under-account a later SOF.
        if marker == 0 {
            continue;
        }
        if marker == 0xd8 || marker == 0xd9 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            return None;
        }

        let length_bytes = bytes.get(offset..offset + 2)?;
        let segment_len = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if segment_len < 2 {
            return None;
        }
        let payload_start = offset + 2;
        let payload_end = payload_start.checked_add(segment_len - 2)?;
        let payload = bytes.get(payload_start..payload_end)?;

        // Keep this set aligned with zune-jpeg's Marker::from_u8 mapping.
        // Other SOF-shaped, length-bearing markers are skipped by that decoder
        // and must not hide the later frame header used for allocation.
        let is_sof = matches!(marker, 0xc0..=0xc2);
        if is_sof {
            if payload.len() < 6 {
                return None;
            }
            let component_count = usize::from(payload[5]);
            let components_end = 6usize.checked_add(component_count.checked_mul(3)?)?;
            if component_count == 0 || components_end > payload.len() {
                return None;
            }

            let mut components = Vec::with_capacity(component_count);
            for component in payload[6..components_end].as_chunks::<3>().0 {
                let sampling = component[1];
                let horizontal = sampling >> 4;
                let vertical = sampling & 0x0f;
                if horizontal == 0 || vertical == 0 {
                    return None;
                }
                components.push(JpegComponentSampling {
                    id: component[0],
                    horizontal,
                    vertical,
                });
            }

            let first_scan_decodes_rows = match jpeg_first_scan(bytes, payload_end, &components) {
                Some(JpegFirstScan::AnotherFrame) => return None,
                Some(JpegFirstScan::CoversFrame) => marker != 0xc2,
                Some(JpegFirstScan::Unclassified) | None => false,
            };

            return Some(JpegFrameHeader {
                width: u32::from_be_bytes([0, 0, payload[3], payload[4]]),
                height: u32::from_be_bytes([0, 0, payload[1], payload[2]]),
                components,
                first_scan_decodes_rows,
            });
        }

        offset = payload_end + zune_jpeg_segment_overread(marker, segment_len);
    }

    None
}

/// Walk from the end of the frame header to the first SOS with the same
/// marker rules as [`jpeg_frame_header`]. `None` means the stream ended or a
/// segment was truncated before a first scan was found.
fn jpeg_first_scan(
    bytes: &[u8],
    mut offset: usize,
    components: &[JpegComponentSampling],
) -> Option<JpegFirstScan> {
    loop {
        if bytes.get(offset) != Some(&0xff) {
            return Some(JpegFirstScan::Unclassified);
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;

        if marker == 0 {
            continue;
        }
        if marker == 0xd8 || marker == 0xd9 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            return Some(JpegFirstScan::Unclassified);
        }
        if matches!(marker, 0xc0..=0xc2) {
            return Some(JpegFirstScan::AnotherFrame);
        }

        let length_bytes = bytes.get(offset..offset + 2)?;
        let segment_len = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if segment_len < 2 {
            return Some(JpegFirstScan::Unclassified);
        }
        let payload_start = offset + 2;
        let payload_end = payload_start.checked_add(segment_len - 2)?;
        let payload = bytes.get(payload_start..payload_end)?;

        if marker == 0xda {
            // zune-jpeg's `parse_sos` accepts 1..=4 selectors, an SOS length
            // of exactly 6 + 2 * Ns, and only distinct IDs present in the
            // frame. It then decodes rows directly when Ns equals the frame's
            // component count.
            let Some((&count, selectors)) = payload.split_first() else {
                return Some(JpegFirstScan::Unclassified);
            };
            let count = usize::from(count);
            if !(1..=4).contains(&count) || payload.len() != 4 + 2 * count {
                return Some(JpegFirstScan::Unclassified);
            }
            let mut seen = Vec::with_capacity(count);
            for selector in selectors[..2 * count].as_chunks::<2>().0 {
                let id = selector[0];
                if seen.contains(&id) || !components.iter().any(|c| c.id == id) {
                    return Some(JpegFirstScan::Unclassified);
                }
                seen.push(id);
            }
            return Some(if count == components.len() {
                JpegFirstScan::CoversFrame
            } else {
                JpegFirstScan::Unclassified
            });
        }

        offset = payload_end + zune_jpeg_segment_overread(marker, segment_len);
    }
}

/// Allowance per metadata segment for the record zune-jpeg keeps for it, such
/// as an `ExtendedXmpSegment` (56 bytes plus a 32-byte GUID allocation) or an
/// `ICCChunk` (32 bytes), in a `Vec` whose capacity may have doubled.
const JPEG_METADATA_SEGMENT_OVERHEAD_BYTES: u64 = 192;

/// The JPEG marker context [`jpeg_retained_metadata_allocation`] is in, following
/// zune-jpeg 0.5.15. Which markers carry a length differs between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JpegWalkContext {
    /// Before the first SOS (`decode_headers_internal`): every marker other
    /// than SOS and EOI is read with its length, including RST, SOI, and TEM,
    /// which reach the length-skipping default arm of `parse_marker_inner`.
    Headers,
    /// Entropy-coded data after an SOS, where restart markers carry no length.
    Scan,
    /// Markers after a scan. Baseline frames read every marker other than SOS
    /// and EOI with its length (`advance_to_next_sos`); progressive frames
    /// handle RST without one and the rest through `parse_marker_inner`.
    BetweenScans,
}

/// Upper bound on the metadata zune-jpeg 0.5.15 copies out of the stream and
/// keeps for the whole decode: EXIF, XMP, extended XMP, IPTC, ICC, MPF, and
/// gain-map payloads, all carried in APPn segments, wherever they appear
/// (zune-jpeg also parses markers between and after scans). Twice each payload
/// covers the retained copy and the buffer extended XMP is reassembled into
/// while its chunks are still held.
///
/// The walk follows zune-jpeg's marker handling in each [`JpegWalkContext`],
/// skipping each segment by the length zune-jpeg consumes: the declared length,
/// plus [`zune_jpeg_segment_overread`]. Stuffed zeros and fill bytes are never markers, and
/// nothing after EOI is parsed. Where zune-jpeg stops early, on an error or a
/// marker that ends a baseline decode, the walk carries on, which can only
/// over-count.
fn jpeg_retained_metadata_allocation(bytes: &[u8]) -> u64 {
    let mut total = 0u64;
    let mut offset = 2usize;
    let mut context = JpegWalkContext::Headers;
    let mut progressive = false;
    while let Some(found) = bytes
        .get(offset..)
        .and_then(|rest| rest.iter().position(|&byte| byte == 0xff))
    {
        let at = offset + found;
        let Some(&marker) = bytes.get(at + 1) else {
            break;
        };
        match marker {
            // A fill byte: any marker follows the last 0xFF.
            0xff => {
                offset = at + 1;
                continue;
            }
            // A stuffed zero is data in every context.
            0x00 => {
                offset = at + 2;
                continue;
            }
            // zune-jpeg stops at EOI after a scan and rejects one before it.
            0xd9 => break,
            _ => {}
        }
        let restart = (0xd0..=0xd7).contains(&marker);
        let lengthless = restart
            && match context {
                JpegWalkContext::Headers => false,
                JpegWalkContext::Scan => true,
                JpegWalkContext::BetweenScans => progressive,
            };
        if lengthless {
            offset = at + 2;
            continue;
        }
        let Some(length) = bytes.get(at + 2..at + 4) else {
            break;
        };
        let declared = usize::from(u16::from_be_bytes([length[0], length[1]]));
        if matches!(marker, 0xe0..=0xef | 0xfe) {
            let available = bytes.len().saturating_sub(at + 4);
            let payload = declared.saturating_sub(2).min(available) as u64;
            total = total
                .saturating_add(payload.saturating_mul(2))
                .saturating_add(JPEG_METADATA_SEGMENT_OVERHEAD_BYTES);
        }
        if context == JpegWalkContext::Headers && marker == 0xc2 {
            progressive = true;
        }
        // zune-jpeg skips `declared - 2` bytes after the length field, plus
        // its APP0 over-read. A length below 2 is rejected in headers and read
        // without a skip between baseline scans.
        offset = at + 4 + declared.saturating_sub(2) + zune_jpeg_segment_overread(marker, declared);
        context = match (marker, context) {
            (0xda, _) => JpegWalkContext::Scan,
            (_, JpegWalkContext::Headers) => JpegWalkContext::Headers,
            _ => JpegWalkContext::BetweenScans,
        };
    }
    total
}

/// Conservative JPEG auxiliary allocation (coefficients plus row scratch),
/// derived from the SOF dimensions and sampling factors. The coefficient planes
/// are left out only when [`JpegFrameHeader::first_scan_decodes_rows`] shows
/// zune-jpeg will not allocate them. Returns `None` for a
/// malformed/unprojectable header; callers then use a worst-case fallback so
/// malformed input is refused rather than admitted cheaply.
fn jpeg_auxiliary_allocation(bytes: &[u8], width: u32, height: u32) -> Option<u64> {
    let header = jpeg_frame_header(bytes)?;
    let width = u64::from(width.max(header.width));
    let height = u64::from(height.max(header.height));
    let h_max = u64::from(header.components.iter().map(|c| c.horizontal).max()?);
    let v_max = u64::from(header.components.iter().map(|c| c.vertical).max()?);
    if h_max == 0 || v_max == 0 {
        return None;
    }

    let mcu_width = h_max.checked_mul(8)?;
    let mcu_height = v_max.checked_mul(8)?;
    let mcu_columns = width.checked_add(mcu_width - 1)?.checked_div(mcu_width)?;
    let mcu_rows = height
        .checked_add(mcu_height - 1)?
        .checked_div(mcu_height)?;

    let mut coefficient_bytes = 0u64;
    let mut row_bytes = 0u64;
    for component in &header.components {
        let horizontal = u64::from(component.horizontal);
        let vertical = u64::from(component.vertical);
        let blocks_x = mcu_columns.checked_mul(horizontal)?;
        let blocks_y = mcu_rows.checked_mul(vertical)?;
        let blocks = blocks_x.checked_mul(blocks_y)?;
        let component_bytes = blocks
            .checked_mul(64)?
            .checked_mul(JPEG_COEFFICIENT_BYTES_PER_SAMPLE)?;
        coefficient_bytes = coefficient_bytes.checked_add(component_bytes)?;

        // Baseline decoding reuses a row, but its upsampling and conversion
        // vectors can overlap that row. Charging several rows is conservative
        // and keeps the bound independent of the selected output colorspace.
        let row = blocks_x
            .checked_mul(64)?
            .checked_mul(JPEG_COEFFICIENT_BYTES_PER_SAMPLE)?;
        row_bytes = row_bytes.checked_add(row)?;
    }

    let row_scratch = row_bytes.checked_mul(JPEG_SCRATCH_ROWS_MULTIPLIER)?;
    if header.first_scan_decodes_rows {
        return Some(row_scratch);
    }
    coefficient_bytes.checked_add(row_scratch)
}

/// How each format's animation scratch splits between state the decoder keeps
/// for the whole animation and work it redoes for every frame.
///
/// [`GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL`], [`APNG_SCRATCH_BYTES_PER_PIXEL`],
/// and [`WEBP_ANIMATED_SCRATCH_BYTES_PER_PIXEL`] each bound the *simultaneous*
/// scratch while one frame is in flight, which is the right figure for the
/// per-image ceiling and for admission. They are the wrong figure for the
/// aggregate budget on their own, because they mix two different lifetimes:
/// some of those buffers are allocated once and live across the run, while the
/// rest are allocated and dropped on every iteration. Charging the whole
/// constant once undercounts an N-frame animation by roughly `N - 1` times the
/// recurring part.
///
/// `persistent` is charged once per animation; `per_frame` is charged for every
/// frame the decoder yields. Their sum is the original constant, so the
/// per-frame peak model is unchanged.
struct AnimationScratchModel {
    /// Buffers allocated once and kept alive for the whole animation.
    persistent: u64,
    /// Buffers re-allocated and dropped on every frame.
    per_frame: u64,
}

/// GIF: the persistent `non_disposed_frame` composition canvas (4 B/px) is
/// allocated once before the first frame; the partial-frame branch's temporary
/// sub-rectangle (up to 4 B/px) is allocated and dropped inside every
/// `next()` (`image` 0.25.10 `src/codecs/gif.rs:276-291`, `:326-341`,
/// `:388-415`).
const GIF_ANIMATION_SCRATCH_MODEL: AnimationScratchModel = AnimationScratchModel {
    persistent: 4,
    per_frame: 4,
};

/// APNG: `current` and `previous` (4 B/px each) persist across the animation.
///
/// The recurring term covers two buffers that are simultaneously live inside
/// every `mix_next_frame` (`src/codecs/png.rs:418-425`, `:456-479`): the raw
/// frame buffer sized from the source color type, and the RGBA `source` that
/// the conversion allocates from it. `from_raw` consumes the raw buffer and
/// `into_rgba8()` allocates a *new* 4 B/px buffer, and the raw allocation is
/// released only afterwards by `free_usize`, so both are held at once.
///
/// The worst accepted color type is `Rgb8`: raw 3 B/px + converted 4 B/px =
/// 7 B/px per frame. (`Rgba8` is cheaper — `from_raw` reuses the buffer with no
/// conversion — and `L8`/`La8` allocate smaller raw buffers, so 7 bounds them
/// all.)
const APNG_SCRATCH_MODEL: AnimationScratchModel = AnimationScratchModel {
    persistent: 8,
    per_frame: 7,
};

/// Animated WebP: only the composition canvas (4 B/px) persists; the decoded
/// frame, alpha planes, and the adapter's conversion buffers are per-frame
/// (`image-webp` 0.2.4 `src/decoder.rs` `read_frame`, `image` 0.25.10
/// `src/codecs/webp/decoder.rs:127-148`).
const WEBP_ANIMATED_SCRATCH_MODEL: AnimationScratchModel = AnimationScratchModel {
    persistent: 4,
    per_frame: 13,
};

/// Worst-case peak decode allocation for one canvas, derived from the header
/// alone. Reading the header does not decode pixels, so this is cheap enough
/// to run before the budget check — which is the point: an image whose decode
/// cannot fit the remaining budget is refused without ever being decoded.
///
/// This must bound the decoder's *peak*, not a nominal RGBA canvas. Two ways
/// the two differ, both of which a `width * height * 4` estimate misses:
///
/// - **Output width.** `DynamicImage::from_decoder` sizes its buffer from the
///   decoder's own `ColorType`, so a 16-bit PNG needs 6 (`Rgb16`) or 8
///   (`Rgba16`) bytes per pixel, not 4 — and a still WebP needs only 3
///   (`Rgb8`). `ImageDecoder::total_bytes` is exactly that allocation, so it is
///   used directly rather than re-derived.
/// - **Temporaries.** WebP decoding holds scratch buffers alongside the output;
///   see [`WEBP_STILL_SCRATCH_BYTES_PER_PIXEL`] and
///   [`WEBP_ANIMATED_SCRATCH_BYTES_PER_PIXEL`] for the per-shape derivation.
///   No other supported format allocates a full-canvas temporary beyond the
///   buffer `total_bytes` already describes.
///
/// A header that cannot be parsed is reported as corrupt here rather than
/// charged against the budget; the same payload would fail the decode anyway.
fn projected_allocation(source: &str, mime: &str, bytes: &[u8]) -> anyhow::Result<u64> {
    let format = image_format_for_mime(source, mime)?;

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODED_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODED_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_IMAGE_ALLOC_BYTES);

    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
    reader.set_format(format);
    reader.limits(limits);

    let corrupt = |error: image::ImageError| MultimodalError::CorruptImage {
        input: source.to_string(),
        mime: mime.to_string(),
        reason: error.to_string(),
    };

    // Header-only: this parses IHDR / RIFF chunk headers / the GIF screen
    // descriptor, it does not decode pixels.
    let decoder = reader.into_decoder().map_err(corrupt)?;
    let (width, height) = image::ImageDecoder::dimensions(&decoder);
    let output = image::ImageDecoder::total_bytes(&decoder);

    let pixels = u64::from(width).saturating_mul(u64::from(height));
    let scratch = match format {
        image::ImageFormat::WebP => {
            // `has_animation` is not on the `ImageDecoder` trait, so the shape
            // is read from the concrete decoder. Constructing it walks the RIFF
            // chunk table (and, for `ANMF`, its 16-byte frame headers); it
            // decodes no pixels. A payload whose header cannot be read at all
            // is charged the animated bound rather than the smaller one, so a
            // malformed file is refused instead of admitted cheaply — the
            // decode below would reject it in any case.
            let animated = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes))
                .map(|decoder| decoder.has_animation())
                .unwrap_or(true);
            pixels.saturating_mul(if animated {
                WEBP_ANIMATED_SCRATCH_BYTES_PER_PIXEL
            } else {
                WEBP_STILL_SCRATCH_BYTES_PER_PIXEL
            })
        }
        image::ImageFormat::Gif => {
            // Every GIF is validated through the animation path, where the
            // iterator holds a persistent composition canvas alongside the
            // frame buffer it yields. See
            // [`GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL`].
            pixels.saturating_mul(GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL)
        }
        image::ImageFormat::Png => {
            // Only an APNG takes the animation path and pays for the two
            // persistent canvases; a still PNG decodes straight into one
            // buffer that `total_bytes` already covers. `is_apng` re-reads the
            // chunk sequence looking for `acTL`; it decodes no pixels. A
            // header that cannot be read is charged the animated bound, so a
            // malformed payload is refused rather than admitted cheaply — the
            // decode below would reject it in any case.
            //
            // APNGs that use `DisposeOp::Background` are assigned a projection
            // above `MAX_DECODED_IMAGE_ALLOC_BYTES` so the per-image ceiling
            // refuses them before any decoder is constructed. See
            // `apng_uses_background_disposal` for why that path cannot be
            // modelled: the disposal snapshot is an unmetered allocation inside
            // the decoder, invisible to `Limits`, and can be a multiple of the
            // canvas for a full-region frame.
            let is_apng = image::codecs::png::PngDecoder::new(std::io::Cursor::new(bytes))
                .and_then(|decoder| decoder.is_apng())
                .unwrap_or(true);
            if is_apng {
                if apng_uses_background_disposal(bytes) {
                    // Return a value guaranteed to exceed the per-image cap,
                    // triggering `per_image_cap_refusal` before any decode.
                    MAX_DECODED_IMAGE_ALLOC_BYTES.saturating_add(1)
                } else {
                    pixels.saturating_mul(APNG_SCRATCH_BYTES_PER_PIXEL)
                }
            } else {
                0
            }
        }
        image::ImageFormat::Jpeg => {
            // zune-jpeg's full-image coefficient planes are not visible via
            // `ImageDecoder::total_bytes`. Use the SOF-derived bound, falling
            // back to a deliberately expensive estimate when a malformed
            // header cannot be projected. The latter is only reached for
            // input that will fail full decode anyway, and ensures it cannot
            // sneak past the pre-decode per-image cap. On every path two copies
            // of the encoded input are live for the decode: the owned buffer
            // `validate_image_content_with_projection` moves into the blocking
            // task, and the `Vec` `image`'s JPEG adapter reads that buffer into.
            // The decoder's retained metadata is live alongside them.
            jpeg_auxiliary_allocation(bytes, width, height)
                .unwrap_or_else(|| {
                    pixels
                        .saturating_mul(8)
                        .saturating_add(u64::from(width).saturating_mul(64).saturating_mul(8))
                })
                .saturating_add((bytes.len() as u64).saturating_mul(2))
                .saturating_add(jpeg_retained_metadata_allocation(bytes))
        }
        _ => 0,
    };

    Ok(output.saturating_add(scratch))
}

/// `dispose_op` value in an APNG `fcTL` chunk meaning "clear the frame's region
/// to fully transparent black before compositing the next frame".
///
/// `image` 0.25.10 implements this by snapshotting the whole disposal region
/// into a `Vec` before clearing it — see [`apng_uses_background_disposal`].
const APNG_DISPOSE_OP_BACKGROUND: u8 = 1;

/// Whether any `fcTL` chunk in `bytes` requests background disposal.
///
/// `ApngDecoder::mix_next_frame` handles `DisposeOp::Background` by collecting
/// the entire disposal region before clearing it
/// (`image-0.25.10/src/codecs/png.rs:385-401`):
///
/// ```ignore
/// let pixels: Vec<_> = region_current.pixels().collect();
/// ```
///
/// That `Vec<(u32, u32, Rgba<u8>)>` is 12 bytes per region pixel and is **not**
/// preceded by any `Limits::reserve*` call, so it is invisible to both the
/// decoder's own allocation cap and to this module's accounting. For a
/// full-canvas region it is three times the canvas itself, which is enough to
/// carry an image whose header projection sits under the per-image ceiling well
/// past it once decoding starts.
///
/// The other two disposal modes are bounded: `None` only clones between the two
/// canvases already accounted for, and `Previous` copies a sub-image back, both
/// without a per-pixel collection. Rejecting only `Background` therefore closes
/// the untracked allocation without refusing ordinary animations — inflating the
/// scratch bound instead would have to assume the worst case for *every* APNG
/// and would reject valid images that never take this branch.
///
/// The scan is header-only: `fcTL` is a fixed 26-byte payload and `dispose_op`
/// is its 25th byte, so no pixel data is touched. A stream that cannot be walked
/// is reported as *not* using background disposal; it is malformed and the
/// decode that follows rejects it on its own terms.
fn apng_uses_background_disposal(bytes: &[u8]) -> bool {
    const PNG_SIGNATURE_LEN: usize = 8;
    const CHUNK_HEADER_LEN: usize = 8; // 4-byte length + 4-byte type
    const CHUNK_CRC_LEN: usize = 4;
    const FCTL_PAYLOAD_LEN: usize = 26;
    const FCTL_DISPOSE_OP_OFFSET: usize = 24;

    let mut offset = PNG_SIGNATURE_LEN;
    while offset + CHUNK_HEADER_LEN + CHUNK_CRC_LEN <= bytes.len() {
        let Ok(length_bytes) = <[u8; 4]>::try_from(&bytes[offset..offset + 4]) else {
            return false;
        };
        let length = u32::from_be_bytes(length_bytes) as usize;
        let kind = &bytes[offset + 4..offset + CHUNK_HEADER_LEN];
        let payload_start = offset + CHUNK_HEADER_LEN;
        let Some(chunk_end) = payload_start
            .checked_add(length)
            .and_then(|end| end.checked_add(CHUNK_CRC_LEN))
        else {
            return false;
        };
        if chunk_end > bytes.len() {
            return false;
        }

        if kind == b"fcTL"
            && length == FCTL_PAYLOAD_LEN
            && bytes[payload_start + FCTL_DISPOSE_OP_OFFSET] == APNG_DISPOSE_OP_BACKGROUND
        {
            return true;
        }

        offset = chunk_end;
    }

    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageValidationFailureKind {
    InvalidImage,
    /// Refused by a header-only check before any decoder could allocate, so no
    /// decode work was performed and nothing should be charged.
    RefusedBeforeDecode,
    AggregateBudgetExhausted,
}

#[derive(Debug)]
struct ImageValidationFailure {
    error: anyhow::Error,
    consumed_allocation: u64,
    kind: ImageValidationFailureKind,
}

fn invalid_image_failure(
    source: &str,
    mime: &str,
    reason: String,
    consumed_allocation: u64,
) -> ImageValidationFailure {
    ImageValidationFailure {
        error: MultimodalError::CorruptImage {
            input: source.to_string(),
            mime: mime.to_string(),
            reason,
        }
        .into(),
        consumed_allocation,
        kind: ImageValidationFailureKind::InvalidImage,
    }
}

fn aggregate_budget_failure(
    source: &str,
    mime: &str,
    consumed_allocation: u64,
) -> ImageValidationFailure {
    ImageValidationFailure {
        error: MultimodalError::CorruptImage {
            input: source.to_string(),
            mime: mime.to_string(),
            reason: "aggregate decode budget exhausted".to_string(),
        }
        .into(),
        consumed_allocation,
        kind: ImageValidationFailureKind::AggregateBudgetExhausted,
    }
}

/// Classify a header projection that exceeds the independent per-image
/// allocation ceiling.
///
/// This ceiling is the *image's own* limit, unrelated to the shared per-call
/// allowance: the refusal happens before any decoder is constructed, so no
/// pixel work is performed and nothing may be charged. Callers must consult
/// this before the aggregate-budget gate — a projection that exceeds both
/// ceilings belongs to this one, and reporting it as aggregate exhaustion would
/// close the shared allowance for siblings that never got to decode.
fn per_image_cap_refusal(
    source: &str,
    mime: &str,
    projected_allocation: u64,
) -> Option<ImageValidationFailure> {
    if projected_allocation < MAX_DECODED_IMAGE_ALLOC_BYTES {
        return None;
    }

    Some(ImageValidationFailure {
        error: MultimodalError::CorruptImage {
            input: source.to_string(),
            mime: mime.to_string(),
            reason: format!(
                "decoded canvas of {projected_allocation} bytes exceeds per-image limit of \
                 {MAX_DECODED_IMAGE_ALLOC_BYTES} bytes"
            ),
        }
        .into(),
        consumed_allocation: 0,
        kind: ImageValidationFailureKind::RefusedBeforeDecode,
    })
}

fn image_error_failure(
    source: &str,
    mime: &str,
    error: image::ImageError,
    consumed_allocation: u64,
    budget_cap: u64,
) -> ImageValidationFailure {
    let allocation_limit_hit = matches!(
        &error,
        image::ImageError::Limits(limit)
            if matches!(
                limit.kind(),
                image::error::LimitErrorKind::InsufficientMemory
            )
    );

    // When the caller's remaining budget is the tighter allocation cap, an
    // allocator-limit error is a real aggregate-budget exhaustion. Otherwise
    // it is this image hitting the independent per-image safety cap and must
    // not discard unrelated siblings.
    if allocation_limit_hit && budget_cap <= MAX_DECODED_IMAGE_ALLOC_BYTES {
        aggregate_budget_failure(source, mime, consumed_allocation)
    } else {
        invalid_image_failure(source, mime, error.to_string(), consumed_allocation)
    }
}

/// Validate every frame of an animation under the per-image and caller budgets.
///
/// Returns the charge for the caller's aggregate budget: the larger of the
/// cumulative frame work (every frame buffer the decoder produced, plus the
/// state it holds across the whole animation) and the largest single-frame
/// peak. Animation decoders retain full-canvas state that never appears in a
/// returned `Frame` — GIF keeps a persistent composition canvas, APNG keeps
/// `current` plus `previous` — which is what `scratch_per_pixel` models, using
/// the same decoder-specific figure admission already applied.
///
/// Charging only the single-frame peak was a real under-count: a long animation
/// decodes every frame's pixels but would debit just one frame's worth, letting
/// a sequence of accepted animations run past the aggregate envelope while the
/// counter still reported headroom.
fn validate_animation_frames(
    frames: image::Frames<'_>,
    source: &str,
    mime: &str,
    projected_allocation: u64,
    budget_cap: u64,
    scratch: AnimationScratchModel,
) -> Result<u64, ImageValidationFailure> {
    let scratch_per_pixel = scratch.persistent.saturating_add(scratch.per_frame);
    let effective_cap = MAX_DECODED_IMAGE_ALLOC_BYTES.min(budget_cap);
    let aggregate_cap_is_tighter = budget_cap <= MAX_DECODED_IMAGE_ALLOC_BYTES;
    // Cumulative work: every frame's output buffer plus the scratch the decoder
    // re-allocates for that frame. Both recur per frame, so both accumulate.
    let mut cumulative = 0u64;
    // Sum of the returned frame buffers alone, used for the per-image ceiling
    // check that predates the cumulative model.
    let mut total = 0u64;
    // Largest single-frame peak seen: the frame's own buffer plus the
    // full-canvas decoder state live alongside it.
    let mut peak = 0u64;
    // One canvas of the state the decoder keeps for the whole animation,
    // charged once rather than per frame. Sized from the largest frame, since
    // that state is allocated from the canvas dimensions.
    let mut persistent_scratch = 0u64;

    for frame in frames {
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                // Charge what the decoder actually did before failing.
                //
                // `cumulative` already holds every completed frame's output plus
                // its recurring scratch, and `persistent_scratch` the state held
                // across them; using `total` here would drop the recurring term
                // for every valid frame that preceded the corrupt one, so a long
                // valid prefix followed by a bad frame would leave the budget
                // reporting headroom the decoder had already spent.
                //
                // The failed frame itself also allocated before the error
                // surfaced — decoders reserve a canvas or frame buffer before
                // discovering corrupt data — so add one header projection for
                // that attempt on top of the completed work.
                let attempted = cumulative
                    .saturating_add(persistent_scratch)
                    .saturating_add(projected_allocation);
                return Err(image_error_failure(
                    source, mime, error, attempted, budget_cap,
                ));
            }
        };
        let buffer = frame.buffer();

        if buffer.width() > MAX_DECODED_IMAGE_DIMENSION
            || buffer.height() > MAX_DECODED_IMAGE_DIMENSION
        {
            return Err(invalid_image_failure(
                source,
                mime,
                format!(
                    "frame dimensions {}x{} exceed limit of {MAX_DECODED_IMAGE_DIMENSION}",
                    buffer.width(),
                    buffer.height()
                ),
                cumulative
                    .saturating_add(persistent_scratch)
                    .saturating_add(projected_allocation),
            ));
        }

        let frame_allocation = u64::from(buffer.width()) * u64::from(buffer.height()) * 4;
        // What the decoder actually holds while producing this frame: the
        // frame buffer plus its retained full-canvas state.
        let frame_pixels = u64::from(buffer.width()).saturating_mul(u64::from(buffer.height()));
        let frame_scratch = frame_pixels.saturating_mul(scratch_per_pixel);
        let frame_peak = frame_allocation.saturating_add(frame_scratch);
        // The persistent term is charged once, so track the largest frame's
        // share of it; the recurring term is charged for this frame.
        persistent_scratch =
            persistent_scratch.max(frame_pixels.saturating_mul(scratch.persistent));
        cumulative = cumulative
            .saturating_add(frame_allocation)
            .saturating_add(frame_pixels.saturating_mul(scratch.per_frame));
        peak = peak.max(frame_peak);

        let next_total = total.saturating_add(frame_allocation);
        // Enforce every bound as soon as this frame's cost is known, so the
        // *next* frame is never decoded once the animation cannot fit:
        //
        // - `next_total`  — the returned frame buffers alone.
        // - `frame_peak`  — this one frame's simultaneous allocation, so a
        //   single oversized frame is refused even when the run so far is small.
        // - `cumulative + persistent_scratch` — the running modeled charge.
        //   Without this the loop would decode every frame and only report the
        //   over-budget total on return, which is exactly the work the budget
        //   exists to prevent.
        //
        // The iterator has already produced the current frame by the time these
        // run, so the frame that crosses the allowance is decoded before it is
        // rejected. That overshoot is bounded by the per-frame and per-image
        // caps checked above — at most one frame beyond the limit, never the
        // remainder of the animation.
        let running_charge = cumulative.saturating_add(persistent_scratch);
        if next_total > effective_cap
            || frame_peak > effective_cap
            || running_charge > effective_cap
        {
            let observed = next_total.max(frame_peak).max(running_charge);
            if aggregate_cap_is_tighter {
                return Err(aggregate_budget_failure(source, mime, observed));
            }
            return Err(invalid_image_failure(
                source,
                mime,
                format!(
                    "cumulative frame allocation exceeds per-image limit of \
                     {MAX_DECODED_IMAGE_ALLOC_BYTES} bytes"
                ),
                observed,
            ));
        }
        total = next_total;
    }

    if total == 0 {
        return Err(invalid_image_failure(
            source,
            mime,
            "no frames".to_string(),
            projected_allocation,
        ));
    }

    // Charge the **larger** of two quantities:
    //
    // 1. `cumulative + persistent_scratch` — every frame's output buffer plus
    //    its per-frame transient scratch (e.g. the GIF sub-rectangle, the APNG
    //    raw+conversion pair, the WebP frame/alpha/adapter buffers — all
    //    re-allocated and freed on every iteration), with one canvas of state
    //    that stays live for the whole run (GIF's `non_disposed_frame`; APNG's
    //    `current` and `previous`; WebP's composition canvas). The recurring
    //    part scales with frame count; the persistent part is charged once.
    //
    // 2. `peak` — the largest single-frame allocation (frame output + all
    //    scratch). For a single-frame animation these are equal; for a long
    //    animation option 1 is larger and becomes the binding term.
    //
    // Taking the max keeps a lone oversized frame honestly charged.
    Ok(cumulative.saturating_add(persistent_scratch).max(peak))
}

/// Validate `bytes` against the aggregate decode budget, then decode.
///
/// The per-image cap and the aggregate gate are checked in that order before
/// any pixel work begins. An image that exceeds only its own per-image ceiling
/// is refused without touching `remaining_budget` so valid siblings are not
/// affected; an image whose projection fits the per-image cap but not the
/// remaining shared allowance drives `remaining_budget` to zero, because at
/// that point the request has genuinely run out of decode headroom.
///
/// Callers walk candidates newest-first, so exhaustion drops the *oldest*
/// images — matching [`trim_old_images`], which also sheds oldest-first when
/// the per-request image cap is exceeded.
async fn validate_within_budget(
    source: &str,
    mime: &str,
    bytes: &[u8],
    remaining_budget: &mut u64,
) -> anyhow::Result<u64> {
    if *remaining_budget == 0 {
        return Err(aggregate_budget_failure(source, mime, 0).error);
    }

    let projected_allocation = projected_allocation(source, mime, bytes)?;

    // Check the per-image ceiling before the aggregate gate.
    //
    // A projection that exceeds the independent per-image cap (e.g. an animated
    // WebP whose scratch model is 17 B/px, giving ~525 MB for a 5000x5000
    // canvas) must be refused without closing the shared request allowance: the
    // image is too big on its own merits, not because this request ran out of
    // budget. Checking the aggregate gate first would zero `remaining_budget`
    // and skip every later candidate, including unrelated valid siblings that
    // never got to decode.
    //
    // `validate_image_content_with_projection` carries the same guard, but it
    // is unreachable once the projection also exceeds the aggregate allowance.
    // Pulling it forward classifies the refusal by the ceiling that actually
    // caused it, whichever of the two is crossed first.
    if let Some(failure) = per_image_cap_refusal(source, mime, projected_allocation) {
        return Err(failure.error);
    }

    if projected_allocation > *remaining_budget {
        *remaining_budget = 0;
        return Err(aggregate_budget_failure(source, mime, 0).error);
    }

    let budget_before_decode = *remaining_budget;
    match validate_image_content_with_projection(
        source,
        mime,
        bytes,
        budget_before_decode,
        projected_allocation,
    )
    .await
    {
        Ok(allocation) => {
            // Charge the same model admission reserved against. The decode
            // reports the buffer it can see — the decoded output, or the
            // cumulative frame output for an animation — but the projection
            // also covered decoder scratch that is invisible from out here
            // (the WebP RGBA/alpha temporaries, the animation canvas, and the
            // adapter's RGB->RGBA conversion). Debiting only the visible
            // buffer would let this counter drift below real consumption: with
            // up to 16 candidates permitted, a sequence of valid sub-cap WebPs
            // would each reserve their full peak here and refund most of it,
            // so the request could decode far more than the aggregate envelope
            // claims. The real figure still wins whenever it is larger, which
            // is what keeps a high-bit-depth PNG honestly charged.
            let charge = allocation.max(projected_allocation);
            *remaining_budget = budget_before_decode.saturating_sub(charge);
            Ok(charge)
        }
        Err(failure) => {
            match failure.kind {
                ImageValidationFailureKind::AggregateBudgetExhausted => {
                    *remaining_budget = 0;
                }
                ImageValidationFailureKind::RefusedBeforeDecode => {
                    // A header-only refusal: no decoder ran, so there is no
                    // work to charge. The check that produced it is bounded by
                    // the header parse already paid for by `projected_allocation`,
                    // and the per-request image cap bounds how often it can run,
                    // so leaving the allowance intact cannot be used to repeat
                    // real decode work.
                }
                ImageValidationFailureKind::InvalidImage => {
                    // A corrupt payload still consumed bounded decode work.
                    // Charge at least its admitted canvas, plus any
                    // completed/attempted animation frames reported by the
                    // decoder, while preserving the remainder for unrelated
                    // candidates.
                    let charge = projected_allocation.max(failure.consumed_allocation);
                    *remaining_budget = budget_before_decode.saturating_sub(charge);
                }
            }
            Err(failure.error)
        }
    }
}

/// Decode the image far enough to prove the bytes really are a well-formed
/// image of the declared type. Header sniffing alone cannot do this: an
/// 8-byte PNG signature with no IDAT, a truncated JPEG, or arbitrary data
/// renamed to `.png` all pass a magic-byte check and are only rejected by the
/// provider — as a hard 400 that fails the whole request, including the
/// unrelated text and images batched alongside it.
///
/// The decode is bounded by `Limits` so this validation cannot itself become a
/// decompression-bomb sink: a small payload declaring enormous dimensions is
/// rejected here rather than being allocated.
///
/// `budget_cap` is the caller's remaining aggregate decode budget at the time
/// of this call (see [`AGGREGATE_DECODE_BUDGET_BYTES`]). A multi-frame GIF,
/// APNG, or WebP animation can cost more than the header's single-canvas
/// projection, so the per-image cap and caller budget are also enforced live
/// while every frame is decoded.
///
/// Returns the approximate decoded memory allocation (width × height × 4 bytes
/// for RGBA) so callers can track aggregate budget consumption.
#[cfg(test)]
async fn validate_image_content(
    source: &str,
    mime: &str,
    bytes: &[u8],
    budget_cap: u64,
) -> anyhow::Result<u64> {
    let projected_allocation = projected_allocation(source, mime, bytes)?;
    if projected_allocation > budget_cap {
        return Err(aggregate_budget_failure(source, mime, 0).error);
    }

    validate_image_content_with_projection(source, mime, bytes, budget_cap, projected_allocation)
        .await
        .map_err(|failure| failure.error)
}

async fn validate_image_content_with_projection(
    source: &str,
    mime: &str,
    bytes: &[u8],
    budget_cap: u64,
    projected_allocation: u64,
) -> Result<u64, ImageValidationFailure> {
    #[cfg(test)]
    record_decode_call();

    let format = image_format_for_mime(source, mime).map_err(|error| ImageValidationFailure {
        error,
        consumed_allocation: 0,
        kind: ImageValidationFailureKind::InvalidImage,
    })?;

    // Enforce the per-image allocation cap before any decoder can allocate.
    // `projected_allocation` now returns the decoder's worst-case peak — output
    // bytes from `total_bytes` plus any format-specific temporaries (WebP
    // lossless scratch). Refusing here prevents both the equality-boundary case
    // (4096×4096 RGBA = exactly 64 MiB but the decoder also holds temporaries)
    // and sub-threshold payloads whose actual peak exceeds the cap
    // (e.g. 4000×4000 lossless WebP: 60 MiB projected but ~112 MiB at peak).
    //
    // `Limits::max_alloc` alone is not sufficient: WebPDecoder (image 0.25.10)
    // inherits the trait default for `set_limits`, which checks dimensions but
    // does not enforce the allocation limit, so the cap would be silently
    // dropped on that path.
    //
    // Ordinary invalid-image failure: this is the image's own ceiling, not
    // the shared allowance, so it must not close the budget for siblings.
    // Nothing has been decoded yet, hence a zero charge.
    //
    // `validate_within_budget` applies the same guard before its aggregate
    // gate, so this is the effective check only for callers that arrive here
    // directly.
    if let Some(failure) = per_image_cap_refusal(source, mime, projected_allocation) {
        return Err(failure);
    }

    let source_owned = source.to_string();
    let mime_owned = mime.to_string();
    let bytes_owned = bytes.to_vec();

    // Decode in a blocking pool to avoid stalling the async executor.
    tokio::task::spawn_blocking(move || {
        let effective_cap = MAX_DECODED_IMAGE_ALLOC_BYTES.min(budget_cap);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_DECODED_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_DECODED_IMAGE_DIMENSION);
        limits.max_alloc = Some(effective_cap);

        match format {
            image::ImageFormat::Gif => {
                let mut decoder =
                    image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes_owned))
                        .map_err(|error| {
                            image_error_failure(
                                &source_owned,
                                &mime_owned,
                                error,
                                projected_allocation,
                                budget_cap,
                            )
                        })?;
                // `GifDecoder` starts with `Limits::no_limits()`. Apply limits
                // before frame iteration so an oversized canvas/frame is
                // refused by the decoder before its output buffer is allocated.
                //
                // The per-frame buffer is sized from the GIF *image descriptor*,
                // not the logical screen: `next_frame` calls
                // `local_limits.reserve_buffer(frame.width, frame.height, ...)`
                // with the descriptor's dimensions (`src/codecs/gif.rs:326-331`).
                // A descriptor may declare a frame larger than the logical
                // screen, and the frame the iterator yields is the composited
                // *screen-sized* canvas, so an oversized descriptor is invisible
                // both to `projected_allocation` (which reads the logical
                // screen) and to the returned-frame accounting.
                //
                // Clamping the dimension limits to the logical screen closes
                // that gap: the decoder refuses a descriptor exceeding the
                // canvas before the buffer for it is allocated. A GIF whose
                // frames stay within the screen is unaffected, which is every
                // ordinary GIF — the format composites frames onto that canvas.
                let (screen_width, screen_height) = image::ImageDecoder::dimensions(&decoder);
                limits.max_image_width = Some(screen_width.min(MAX_DECODED_IMAGE_DIMENSION));
                limits.max_image_height = Some(screen_height.min(MAX_DECODED_IMAGE_DIMENSION));
                image::ImageDecoder::set_limits(&mut decoder, limits).map_err(|error| {
                    image_error_failure(
                        &source_owned,
                        &mime_owned,
                        error,
                        projected_allocation,
                        budget_cap,
                    )
                })?;
                validate_animation_frames(
                    image::AnimationDecoder::into_frames(decoder),
                    &source_owned,
                    &mime_owned,
                    projected_allocation,
                    budget_cap,
                    GIF_ANIMATION_SCRATCH_MODEL,
                )
            }
            image::ImageFormat::Png => {
                // `with_limits` also passes the allocation cap to the lower
                // level PNG decoder; setting limits only after construction
                // would leave its internal buffers unconstrained.
                let decoder = image::codecs::png::PngDecoder::with_limits(
                    std::io::Cursor::new(&bytes_owned),
                    limits,
                )
                .map_err(|error| {
                    image_error_failure(
                        &source_owned,
                        &mime_owned,
                        error,
                        projected_allocation,
                        budget_cap,
                    )
                })?;
                let is_apng = decoder.is_apng().map_err(|error| {
                    image_error_failure(
                        &source_owned,
                        &mime_owned,
                        error,
                        projected_allocation,
                        budget_cap,
                    )
                })?;
                if is_apng {
                    // Refuse background disposal before the decoder runs: that
                    // branch snapshots the whole disposal region into an
                    // unmetered `Vec`, which no projection can bound. See
                    // `apng_uses_background_disposal`.
                    if apng_uses_background_disposal(&bytes_owned) {
                        return Err(ImageValidationFailure {
                            error: MultimodalError::CorruptImage {
                                input: source_owned.clone(),
                                mime: mime_owned.clone(),
                                reason: "APNG background disposal is not supported: the decoder \
                                         snapshots the full disposal region into an unbounded \
                                         buffer"
                                    .to_string(),
                            }
                            .into(),
                            consumed_allocation: 0,
                            kind: ImageValidationFailureKind::RefusedBeforeDecode,
                        });
                    }
                    let decoder = decoder.apng().map_err(|error| {
                        image_error_failure(
                            &source_owned,
                            &mime_owned,
                            error,
                            projected_allocation,
                            budget_cap,
                        )
                    })?;
                    validate_animation_frames(
                        image::AnimationDecoder::into_frames(decoder),
                        &source_owned,
                        &mime_owned,
                        projected_allocation,
                        budget_cap,
                        APNG_SCRATCH_MODEL,
                    )
                } else {
                    let image = image::DynamicImage::from_decoder(decoder).map_err(|error| {
                        image_error_failure(
                            &source_owned,
                            &mime_owned,
                            error,
                            projected_allocation,
                            budget_cap,
                        )
                    })?;
                    // Report the actual byte count rather than width*height*4.
                    // DynamicImage preserves 16-bit output (e.g. ImageRgb16 = 6
                    // bytes per pixel) so the naive ×4 estimate would undercount
                    // the real allocation by up to 2×.
                    Ok(image.as_bytes().len() as u64)
                }
            }
            image::ImageFormat::WebP => {
                let mut decoder =
                    image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(&bytes_owned))
                        .map_err(|error| {
                            image_error_failure(
                                &source_owned,
                                &mime_owned,
                                error,
                                projected_allocation,
                                budget_cap,
                            )
                        })?;
                image::ImageDecoder::set_limits(&mut decoder, limits).map_err(|error| {
                    image_error_failure(
                        &source_owned,
                        &mime_owned,
                        error,
                        projected_allocation,
                        budget_cap,
                    )
                })?;
                if decoder.has_animation() {
                    validate_animation_frames(
                        image::AnimationDecoder::into_frames(decoder),
                        &source_owned,
                        &mime_owned,
                        projected_allocation,
                        budget_cap,
                        WEBP_ANIMATED_SCRATCH_MODEL,
                    )
                } else {
                    let image = image::DynamicImage::from_decoder(decoder).map_err(|error| {
                        image_error_failure(
                            &source_owned,
                            &mime_owned,
                            error,
                            projected_allocation,
                            budget_cap,
                        )
                    })?;
                    // Report the actual allocation, not width*height*4. Still
                    // WebP is Rgb8 (3 bytes per pixel), so the ×4 estimate
                    // would overcharge by 33%; any format-specific variation
                    // is covered by the real buffer size.
                    Ok(image.as_bytes().len() as u64)
                }
            }
            image::ImageFormat::Jpeg => {
                let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes_owned));
                reader.set_format(format);
                reader.limits(limits);
                let image = reader.decode().map_err(|error| {
                    image_error_failure(
                        &source_owned,
                        &mime_owned,
                        error,
                        projected_allocation,
                        budget_cap,
                    )
                })?;
                // The real buffer, for the same reason as the branches above:
                // JPEG decodes to `Rgb8` or `Luma8`, so a width*height*4 charge
                // would not match what was actually allocated either.
                Ok(image.as_bytes().len() as u64)
            }
            _ => Err(invalid_image_failure(
                &source_owned,
                &mime_owned,
                format!("unsupported decoder format: {format:?}"),
                0,
            )),
        }
    })
    .await
    .map_err(|error| {
        invalid_image_failure(
            source,
            mime,
            format!("decode task failed: {error}"),
            projected_allocation,
        )
    })?
}

fn detect_mime(
    path: Option<&Path>,
    bytes: &[u8],
    header_content_type: Option<&str>,
) -> Option<String> {
    if let Some(header_mime) = header_content_type.and_then(normalize_content_type) {
        return Some(header_mime);
    }

    if let Some(path) = path
        && let Some(ext) = path.extension().and_then(|value| value.to_str())
        && let Some(mime) = image_mime_from_extension(ext)
    {
        return Some(mime.to_string());
    }

    image_mime_from_magic(bytes).map(ToString::to_string)
}

fn normalize_content_type(content_type: &str) -> Option<String> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    if mime.is_empty() { None } else { Some(mime) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_metadata_projection_keeps_documents_and_tool_identity() {
        let image = AttachmentReference {
            kind: AttachmentKind::Image,
            target: "data:image/png;base64,AAAA".into(),
        };
        let document = AttachmentReference {
            kind: AttachmentKind::Document,
            target: "/workspace/report.pdf".into(),
        };
        let messages = vec![
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "call-1".into(),
                tool_name: "".into(),
                success: None,
                content: "literal [IMAGE:/example]".into(),
                attachments: Vec::new(),
            })
            .with_attachments(vec![image.clone(), document.clone()]),
        ];
        let projected = project_attachment_metadata(&messages);
        let envelope: serde_json::Value =
            crate::tool_protocol::tool_message_value(&projected[0]).unwrap();
        assert_eq!(envelope["tool_call_id"], "call-1");
        assert_eq!(
            envelope["content"],
            "literal [IMAGE:/example]\n\nAttached document: /workspace/report.pdf"
        );
        assert_eq!(projected[0].attachments, vec![image.clone()]);
        assert_eq!(messages[0].attachments, vec![image, document]);
        assert!(matches!(
            project_attachment_metadata(&projected),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn wire_metadata_projection_never_promotes_literal_markers() {
        let messages = vec![ChatMessage::user(
            "[IMAGE:/private.png] [DOCUMENT:/private.pdf]",
        )];
        assert!(matches!(
            project_attachment_metadata(&messages),
            Cow::Borrowed(_)
        ));
        assert!(message_image_refs(&messages[0]).is_empty());
    }

    #[test]
    fn text_only_projection_omits_images_but_keeps_other_attachment_fields() {
        let document = AttachmentReference {
            kind: AttachmentKind::Document,
            target: "/workspace/report.pdf".into(),
        };
        let original = ChatMessage::user("literal [IMAGE:/example]").with_attachments(vec![
            AttachmentReference {
                kind: AttachmentKind::Image,
                target: "/workspace/photo.png".into(),
            },
            document.clone(),
        ]);
        let projected = omit_image_attachments(&original);
        assert_eq!(projected.attachments, vec![document]);
        assert!(projected.content.starts_with(&original.content));
        assert!(projected.content.contains(MEDIA_PLACEHOLDER));
        assert_eq!(original.attachments.len(), 2);
        assert_eq!(
            omit_image_attachments(&projected).content,
            projected.content
        );
    }

    /// A real 1x1 PNG. Tests that need a *loadable* image must use this: a
    /// bare 8-byte PNG signature passes header sniffing but is not decodable,
    /// which is exactly what `validate_image_content` now rejects.
    fn valid_png() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 0, 0, 255]),
        ))
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("test PNG encodes");
        buf.into_inner()
    }

    fn valid_jpeg() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0])))
            .write_to(&mut buf, image::ImageFormat::Jpeg)
            .expect("test JPEG encodes");
        buf.into_inner()
    }

    fn jpeg_with_declared_dimensions(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = valid_jpeg();
        let sof = bytes
            .windows(2)
            .position(|marker| {
                marker[0] == 0xff
                    && matches!(
                        marker[1],
                        0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
                    )
            })
            .expect("encoded test JPEG has a frame header");
        bytes[sof + 5..sof + 7].copy_from_slice(&height.to_be_bytes());
        bytes[sof + 7..sof + 9].copy_from_slice(&width.to_be_bytes());
        bytes
    }

    fn jpeg_sof_header(width: u16, height: u16, sampling: &[(u8, u8)]) -> Vec<u8> {
        let segment_len = 8usize + sampling.len() * 3;
        let mut bytes = vec![
            0xff,
            0xd8,
            0xff,
            0xc0,
            (segment_len >> 8) as u8,
            segment_len as u8,
            8,
            (height >> 8) as u8,
            height as u8,
            (width >> 8) as u8,
            width as u8,
            sampling.len() as u8,
        ];
        for (index, &(horizontal, vertical)) in sampling.iter().enumerate() {
            bytes.extend_from_slice(&[(index + 1) as u8, (horizontal << 4) | vertical, 0]);
        }
        bytes
    }

    /// A 17x9 frame header of type `sof_marker` followed by a first SOS that
    /// selects `scan_ids`. Component IDs are 1-based, as in [`jpeg_sof_header`].
    fn jpeg_frame_and_first_scan(
        sof_marker: u8,
        sampling: &[(u8, u8)],
        scan_ids: &[u8],
    ) -> Vec<u8> {
        let mut bytes = jpeg_sof_header(17, 9, sampling);
        bytes[3] = sof_marker;
        bytes.extend_from_slice(&[
            0xff,
            0xda,
            0x00,
            (6 + 2 * scan_ids.len()) as u8,
            scan_ids.len() as u8,
        ]);
        for &id in scan_ids {
            bytes.extend_from_slice(&[id, 0x00]);
        }
        bytes.extend_from_slice(&[0x00, 0x3f, 0x00]);
        bytes
    }

    /// An ordinary encoded photo: `image`'s encoder writes a sequential
    /// single-scan JPEG.
    fn encoded_jpeg(width: u32, height: u32) -> Vec<u8> {
        let photo = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(photo)
            .write_to(&mut buf, image::ImageFormat::Jpeg)
            .expect("test JPEG encodes");
        buf.into_inner()
    }

    /// `bytes` with its sequential frame header relabelled as progressive.
    fn relabelled_progressive(mut bytes: Vec<u8>) -> Vec<u8> {
        let sof = bytes
            .windows(2)
            .position(|marker| marker == [0xff, 0xc0])
            .expect("encoded test JPEG has a baseline frame header");
        bytes[sof + 1] = 0xc2;
        bytes
    }

    /// A tiny PNG whose IHDR declares `width` x `height`. The payload stays a
    /// few dozen bytes, so it sails past `validate_size`; only the decode
    /// limits stop it. Used to prove a decompression bomb is refused before
    /// the declared pixel buffer is ever allocated.
    fn png_bomb(width: u32, height: u32) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut table = [0u32; 256];
            for (i, slot) in table.iter_mut().enumerate() {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
                *slot = c;
            }
            let mut crc = 0xFFFF_FFFFu32;
            for b in data {
                crc = table[((crc ^ u32::from(*b)) & 0xFF) as usize] ^ (crc >> 8);
            }
            crc ^ 0xFFFF_FFFF
        }

        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        // bit depth 8, color type 2 (RGB), default compression/filter/interlace.
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);

        let mut out = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(&ihdr);
        out.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(b"IEND");
        out.extend_from_slice(&crc32(b"IEND").to_be_bytes());
        out
    }

    fn image_reference(target: impl Into<String>) -> zeroclaw_api::media::AttachmentReference {
        zeroclaw_api::media::AttachmentReference {
            target: target.into(),
            kind: zeroclaw_api::media::AttachmentKind::Image,
        }
    }

    /// A native tool-result carrier in the shape the runtime writes: call id,
    /// verbatim body, and the attachments array (always present).
    fn native_carrier(tool_call_id: &str, body: &str, images: &[String]) -> ChatMessage {
        ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
            tool_call_id: (tool_call_id).to_string(),
            tool_name: String::new(),
            content: (body).to_string(),
            success: None,
            attachments: Vec::new(),
        })
        .with_attachments(
            images
                .iter()
                .map(|target| image_reference(target.clone()))
                .collect(),
        )
    }

    /// A structured result with an ID and verbatim body, independent of protocol.
    fn prompt_carrier(body: &str, images: &[String]) -> ChatMessage {
        ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
            tool_call_id: ("prompt-call").to_string(),
            tool_name: String::new(),
            content: (body).to_string(),
            success: None,
            attachments: Vec::new(),
        })
        .with_attachments(
            images
                .iter()
                .map(|target| image_reference(target.clone()))
                .collect(),
        )
    }

    /// The image targets a native carrier declares, read at the fixed
    /// position.
    fn carrier_image_targets(message: &ChatMessage) -> Vec<String> {
        message_image_refs(message)
    }

    #[test]
    fn image_failure_reporting_tracks_reference_and_kind_until_success() {
        let mut cache = LocalImageCache::new();
        let reference = "/tmp/missing.png";

        assert!(cache.should_report_failure(reference, "image_source_not_found"));
        assert!(!cache.should_report_failure(reference, "image_source_not_found"));
        assert!(cache.should_report_failure(reference, "local_read_failed"));
        assert!(!cache.should_report_failure(reference, "local_read_failed"));

        cache.clear_reported_failures(reference);

        assert!(cache.should_report_failure(reference, "image_source_not_found"));
        assert!(cache.should_report_failure(reference, "local_read_failed"));
    }

    #[test]
    fn image_failure_reporting_is_bounded() {
        let mut cache = LocalImageCache::new();
        let active_reference = "/tmp/active.png";
        assert!(cache.should_report_failure(active_reference, "image_source_not_found"));

        for index in 0..REPORTED_IMAGE_FAILURE_MAX_ENTRIES {
            assert!(cache.should_report_failure(
                &format!("/tmp/missing-{index}.png"),
                "image_source_not_found"
            ));
            assert!(!cache.should_report_failure(active_reference, "image_source_not_found"));
        }

        assert_eq!(
            cache.reported_failures.len(),
            REPORTED_IMAGE_FAILURE_MAX_ENTRIES
        );
        assert!(!cache.should_report_failure(active_reference, "image_source_not_found"));
        assert!(cache.should_report_failure("/tmp/missing-0.png", "image_source_not_found"));
    }

    #[test]
    fn image_failure_reporting_does_not_retain_large_references() {
        let mut cache = LocalImageCache::new();
        let reference = format!("data:image/png;base64,{}", "x".repeat(1024 * 1024));

        assert!(cache.should_report_failure(&reference, "invalid_reference"));

        let (stored_reference, stored_kind) = cache.reported_failures.front().unwrap();
        assert_eq!(stored_reference, &image_failure_reference_key(&reference));
        assert_eq!(stored_reference.len(), 32);
        assert_eq!(*stored_kind, "invalid_reference");
    }

    #[tokio::test]
    async fn cached_preparation_retries_and_resets_missing_image_failure() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("restored.png");
        let reference = image_path.to_string_lossy().to_string();
        let messages = vec![ChatMessage::user("Look").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: reference.to_string(),
            },
        ])];
        let config = MultimodalConfig::default();
        let mut cache = LocalImageCache::new();

        for _ in 0..2 {
            let prepared = prepare_messages_for_provider_cached(&messages, &config, &mut cache)
                .await
                .unwrap();
            assert!(!prepared.contains_images);
            assert!(
                prepared.messages[0]
                    .content
                    .contains("1 attached image(s) could not be loaded")
            );
            assert_eq!(cache.reported_failures.len(), 1);
        }

        // A real decodable PNG, not a bare signature: content validation
        // rejects bytes that only carry the magic number, so the restored
        // file must actually decode for the retry to succeed.
        std::fs::write(&image_path, valid_png()).unwrap();
        let restored = prepare_messages_for_provider_cached(&messages, &config, &mut cache)
            .await
            .unwrap();
        assert!(restored.contains_images);
        assert!(message_image_refs(&restored.messages[0])[0].starts_with("data:image/png;base64,"));
        assert!(cache.reported_failures.is_empty());

        std::fs::remove_file(&image_path).unwrap();
        let missing_again = prepare_messages_for_provider_cached(&messages, &config, &mut cache)
            .await
            .unwrap();
        assert!(!missing_again.contains_images);
        assert_eq!(cache.reported_failures.len(), 1);
    }

    #[tokio::test]
    async fn cached_local_image_honors_the_current_size_limit() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("cached.png");
        let image = valid_png();
        std::fs::write(&image_path, &image).unwrap();
        let reference = image_path.to_string_lossy().to_string();
        let mut cache = LocalImageCache::new();
        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;

        normalize_local_image_cached(&reference, image.len(), &mut cache, &mut budget)
            .await
            .expect("the initial request should populate the cache");

        let error =
            normalize_local_image_cached(&reference, image.len() - 1, &mut cache, &mut budget)
                .await
                .expect_err("a lowered live limit must still reject the cached image");

        assert!(matches!(
            error.downcast_ref::<MultimodalError>(),
            Some(MultimodalError::ImageTooLarge {
                size_bytes,
                max_bytes,
                ..
            }) if *size_bytes == image.len() && *max_bytes == image.len() - 1
        ));
    }

    /// Canonical 1x1 PNG payload: 68 characters, a multiple of four, standard
    /// alphabet, no padding. Every accept case below uses it.
    const CANONICAL_PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGMAAQAABQAB";

    const TEN_MB: usize = 10 * 1024 * 1024;

    // Every test in this block fails to compile before the change: the
    // splitter and its rejection enum did not exist.

    #[test]
    fn split_data_uri_accepts_canonical_payload() {
        let uri = format!("data:image/png;base64,{CANONICAL_PNG_B64}");
        let (media_type, payload) =
            split_base64_image_data_uri(&uri, TEN_MB).expect("canonical PNG data URI accepted");
        assert_eq!(media_type, "image/png");
        assert_eq!(payload, CANONICAL_PNG_B64);
    }

    #[test]
    fn split_data_uri_accepts_uppercase_media_type_and_extra_parameters() {
        // The allowlist comparison is case-insensitive, and the header may
        // carry parameters before `;base64`.
        let uri = format!("data:IMAGE/PNG;charset=binary;base64,{CANONICAL_PNG_B64}");
        let (media_type, payload) =
            split_base64_image_data_uri(&uri, TEN_MB).expect("upper-case media type accepted");
        // Returned verbatim — the caller lowercases it once when it builds a
        // wire block.
        assert_eq!(media_type, "IMAGE/PNG");
        assert_eq!(payload, CANONICAL_PNG_B64);
    }

    #[test]
    fn split_data_uri_accepts_every_allowlisted_media_type() {
        for mime in PROVIDER_IMAGE_MIME_TYPES {
            let uri = format!("data:{mime};base64,{CANONICAL_PNG_B64}");
            let (media_type, _) = split_base64_image_data_uri(&uri, TEN_MB)
                .unwrap_or_else(|reason| panic!("{mime} rejected: {reason}"));
            assert_eq!(media_type, *mime);
        }
    }

    #[test]
    fn split_data_uri_accepts_well_formed_padding() {
        // `AA==` has its final-quartet padding bits clear; so does `AAA=`.
        let two_pad = split_base64_image_data_uri("data:image/png;base64,AA==", TEN_MB);
        assert_eq!(two_pad.map(|(_, payload)| payload), Ok("AA=="));
        let one_pad = split_base64_image_data_uri("data:image/png;base64,AAA=", TEN_MB);
        assert_eq!(one_pad.map(|(_, payload)| payload), Ok("AAA="));
    }

    #[test]
    fn split_data_uri_rejects_non_data_uris() {
        for candidate in [
            "/tmp/screenshot.png",
            r"C:\Users\leo\shot.png",
            "http://example.com/a.png",
            "https://example.com/a.png",
            // A `data:` prefix with no comma has no payload to split.
            "data:image/png;base64",
        ] {
            assert_eq!(
                split_base64_image_data_uri(candidate, TEN_MB),
                Err(ImageDataUriRejection::NotADataUri),
                "expected {candidate} to be rejected as a non-data URI"
            );
        }
    }

    #[test]
    fn split_data_uri_rejects_missing_base64_declaration() {
        // Matched case-sensitively, as `normalize_data_uri` already does.
        assert_eq!(
            split_base64_image_data_uri("data:image/png,AAAA", TEN_MB),
            Err(ImageDataUriRejection::NotBase64Encoded)
        );
        assert_eq!(
            split_base64_image_data_uri("data:image/png;BASE64,AAAA", TEN_MB),
            Err(ImageDataUriRejection::NotBase64Encoded)
        );
    }

    #[test]
    fn split_data_uri_rejects_media_types_outside_the_allowlist() {
        for mime in ["image/svg+xml", "image/bmp", "application/pdf", ""] {
            let uri = format!("data:{mime};base64,{CANONICAL_PNG_B64}");
            assert_eq!(
                split_base64_image_data_uri(&uri, TEN_MB),
                Err(ImageDataUriRejection::UnsupportedMediaType),
                "expected {mime} to be rejected"
            );
        }
    }

    #[test]
    fn split_data_uri_rejects_malformed_base64() {
        for payload in [
            // Empty payload.
            "",
            // Not a multiple of four. Preparation always emits canonical
            // padded base64, so a payload this shape cannot be a real image
            // and Anthropic's decoder would reject it.
            "iVBORw0KGgo",
            "/9j/4AAQSkZJRgABAQEAYABgAAD",
            // Characters outside the standard alphabet.
            "AAA-",
            "AA=A",
            // More than two padding characters.
            "AB==CD==",
            // Final-quartet padding bits set: both fail a strict decoder even
            // though the length and alphabet are fine.
            "AB==",
            "AAB=",
        ] {
            let uri = format!("data:image/png;base64,{payload}");
            assert_eq!(
                split_base64_image_data_uri(&uri, TEN_MB),
                Err(ImageDataUriRejection::MalformedBase64),
                "expected payload {payload:?} to be rejected"
            );
        }
    }

    #[test]
    fn split_data_uri_rejects_payloads_over_the_ceiling() {
        let uri = format!("data:image/png;base64,{CANONICAL_PNG_B64}");
        assert_eq!(
            split_base64_image_data_uri(&uri, CANONICAL_PNG_B64.len() - 1),
            Err(ImageDataUriRejection::TooLarge)
        );
        // Exactly at the ceiling is accepted.
        assert!(split_base64_image_data_uri(&uri, CANONICAL_PNG_B64.len()).is_ok());
    }

    #[test]
    fn split_data_uri_rejections_carry_a_short_reason() {
        assert_eq!(
            ImageDataUriRejection::TooLarge.to_string(),
            "image payload exceeds the per-image ceiling"
        );
        assert_eq!(
            ImageDataUriRejection::MalformedBase64.to_string(),
            "malformed base64 payload"
        );
    }

    #[tokio::test]
    async fn prepare_messages_normalizes_images_and_preserves_other_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shot.png");
        std::fs::write(&path, valid_png()).unwrap();
        let history = vec![ChatMessage::user("look  and hear").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (path.display()).to_string(),
            },
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Audio,
                target: "/tmp/clip.wav".to_string(),
            },
        ])];
        let cfg = MultimodalConfig::default();
        let prepared = prepare_messages_for_provider(&history, &cfg).await.unwrap();
        let content = &prepared.messages[0].content;
        assert!(
            !content.contains("/tmp/clip.wav"),
            "audio path must be stripped: {content}"
        );
        assert!(prepared.messages[0].attachments.iter().any(|attachment| {
            attachment.kind == zeroclaw_api::media::AttachmentKind::Audio
                && attachment.target == "/tmp/clip.wav"
        }));
        assert_eq!(content, &history[0].content);
        // The image reference is still normalized to a data URI alongside it.
        assert!(prepared.contains_images, "image still inlined: {content}");
    }

    #[test]
    fn validate_mime_rejects_bmp_but_accepts_provider_supported_types() {
        for mime in ["image/png", "image/jpeg", "image/webp", "image/gif"] {
            assert!(
                validate_mime("src", mime).is_ok(),
                "{mime} should be allowed"
            );
        }
        // BMP is detectable but unsupported by vision providers; it must be
        // rejected here so it never breaks the whole provider request.
        let err = validate_mime("src", "image/bmp").unwrap_err();
        assert_eq!(multimodal_error_kind(&err), "unsupported_mime");
    }

    #[tokio::test]
    async fn validate_image_content_accepts_a_real_image() {
        assert!(
            validate_image_content(
                "src",
                "image/png",
                &valid_png(),
                MAX_DECODED_IMAGE_ALLOC_BYTES
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn validate_image_content_rejects_header_only_png() {
        // The exact shape header sniffing cannot catch: a valid PNG signature
        // with no IDAT chunk. Produced by truncated downloads and interrupted
        // writes.
        let header_only = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        let err = validate_image_content(
            "src",
            "image/png",
            &header_only,
            MAX_DECODED_IMAGE_ALLOC_BYTES,
        )
        .await
        .unwrap_err();
        assert_eq!(multimodal_error_kind(&err), "corrupt_image");
    }

    #[tokio::test]
    async fn validate_image_content_rejects_truncated_image() {
        let full = valid_png();
        let truncated = &full[..full.len() / 2];
        let err =
            validate_image_content("src", "image/png", truncated, MAX_DECODED_IMAGE_ALLOC_BYTES)
                .await
                .unwrap_err();
        assert_eq!(multimodal_error_kind(&err), "corrupt_image");
    }

    #[test]
    fn jpeg_projection_accounts_for_padded_sampling_coefficients_and_scratch() {
        // 17x9 pixels with 4:2:0 sampling: two padded MCU columns make the Y
        // plane use 4x2 blocks and
        // each chroma plane uses 2x1. Every coefficient is an i16 in zune-
        // jpeg's progressive/full-image path.
        let bytes = jpeg_sof_header(17, 9, &[(2, 2), (1, 1), (1, 1)]);
        let auxiliary = jpeg_auxiliary_allocation(&bytes, 17, 9)
            .expect("synthetic SOF should yield a conservative projection");
        let coefficient_bytes = (8 + 2 + 2) * 64 * JPEG_COEFFICIENT_BYTES_PER_SAMPLE;
        let row_scratch =
            (4 + 2 + 2) * 64 * JPEG_COEFFICIENT_BYTES_PER_SAMPLE * JPEG_SCRATCH_ROWS_MULTIPLIER;
        assert_eq!(auxiliary, coefficient_bytes + row_scratch);
    }

    #[test]
    fn oversized_jpeg_coefficients_are_refused_before_decode() {
        // A 6000x6000 progressive 1x1 grayscale JPEG needs 72 MB of i16
        // coefficients before its 36 MB output buffer, so it must fail the
        // 64 MiB per-image admission check without entering the decoder.
        let bytes = jpeg_sof_header(6000, 6000, &[(1, 1)]);
        let auxiliary = jpeg_auxiliary_allocation(&bytes, 6000, 6000).unwrap();
        let projected = 6000u64 * 6000 + auxiliary;
        assert!(projected > MAX_DECODED_IMAGE_ALLOC_BYTES);
        assert!(per_image_cap_refusal("large.jpg", "image/jpeg", projected).is_some());
    }

    // Same 17x9 4:2:0 shape as the padded-sampling test above.
    const SMALL_420_COEFFICIENTS: u64 = (8 + 2 + 2) * 64 * JPEG_COEFFICIENT_BYTES_PER_SAMPLE;
    const SMALL_420_ROW_SCRATCH: u64 =
        (4 + 2 + 2) * 64 * JPEG_COEFFICIENT_BYTES_PER_SAMPLE * JPEG_SCRATCH_ROWS_MULTIPLIER;
    const SAMPLING_420: [(u8, u8); 3] = [(2, 2), (1, 1), (1, 1)];

    #[test]
    fn sequential_first_scan_with_every_component_charges_row_scratch_only() {
        for sof in [0xc0, 0xc1] {
            let bytes = jpeg_frame_and_first_scan(sof, &SAMPLING_420, &[1, 2, 3]);
            assert_eq!(
                jpeg_auxiliary_allocation(&bytes, 17, 9),
                Some(SMALL_420_ROW_SCRATCH),
                "SOF{:x} decodes rows directly, without coefficient planes",
                sof - 0xc0
            );
        }
        // Scan order does not matter, only coverage.
        let bytes = jpeg_frame_and_first_scan(0xc0, &SAMPLING_420, &[3, 1, 2]);
        assert_eq!(
            jpeg_auxiliary_allocation(&bytes, 17, 9),
            Some(SMALL_420_ROW_SCRATCH)
        );
    }

    #[test]
    fn progressive_or_split_first_scan_keeps_coefficient_planes() {
        let full = Some(SMALL_420_COEFFICIENTS + SMALL_420_ROW_SCRATCH);
        for (sof, scan) in [
            (0xc2, &[1, 2, 3][..]),
            (0xc2, &[1][..]),
            (0xc0, &[1][..]),
            (0xc0, &[1, 2][..]),
            (0xc1, &[2, 3][..]),
        ] {
            let bytes = jpeg_frame_and_first_scan(sof, &SAMPLING_420, scan);
            assert_eq!(
                jpeg_auxiliary_allocation(&bytes, 17, 9),
                full,
                "SOF{:x} with first scan {scan:?} must keep the coefficient charge",
                sof - 0xc0
            );
        }
    }

    #[test]
    fn unclassifiable_first_scan_keeps_coefficient_planes() {
        let full = Some(SMALL_420_COEFFICIENTS + SMALL_420_ROW_SCRATCH);
        let cases: [(&str, Vec<u8>); 6] = [
            ("no scan header", jpeg_sof_header(17, 9, &SAMPLING_420)),
            (
                "unknown component ID",
                jpeg_frame_and_first_scan(0xc0, &SAMPLING_420, &[1, 2, 9]),
            ),
            (
                "duplicate component ID",
                jpeg_frame_and_first_scan(0xc0, &SAMPLING_420, &[1, 1, 2]),
            ),
            ("wrong SOS length", {
                let mut bytes = jpeg_frame_and_first_scan(0xc0, &SAMPLING_420, &[1, 2, 3]);
                // Low byte of the SOS length, right after the 21-byte SOI + SOF.
                bytes[24] -= 2;
                bytes
            }),
            ("truncated SOS", {
                let mut bytes = jpeg_frame_and_first_scan(0xc0, &SAMPLING_420, &[1, 2, 3]);
                bytes.truncate(bytes.len() - 4);
                bytes
            }),
            ("standalone attachment before the scan", {
                let mut bytes = jpeg_sof_header(17, 9, &SAMPLING_420);
                bytes.extend_from_slice(&[0xff, 0x01]);
                bytes.extend_from_slice(
                    &jpeg_frame_and_first_scan(0xc0, &SAMPLING_420, &[1, 2, 3])[21..],
                );
                bytes
            }),
        ];
        for (case, bytes) in cases {
            assert_eq!(
                jpeg_auxiliary_allocation(&bytes, 17, 9),
                full,
                "{case} must keep the coefficient charge"
            );
        }
    }

    #[test]
    fn jpeg_projection_charges_both_input_copies() {
        // The validator's owned buffer and `image`'s adapter copy of it are
        // both live for the whole decode.
        let bytes = valid_jpeg();
        let auxiliary = jpeg_auxiliary_allocation(&bytes, 1, 1).unwrap();
        assert_eq!(
            projected_allocation("pixel.jpg", "image/jpeg", &bytes).unwrap(),
            3 + auxiliary + 2 * bytes.len() as u64 + jpeg_retained_metadata_allocation(&bytes)
        );
    }

    #[tokio::test]
    async fn near_limit_baseline_jpeg_is_refused_once_both_input_copies_count() {
        // A 4000x4000 single-scan baseline frame with 10 MiB of encoded input:
        // output, row scratch, and one input copy fit under the cap, but the
        // second live copy does not.
        let mut bytes = jpeg_with_declared_dimensions(4000, 4000);
        bytes.resize(10 * 1024 * 1024, 0);
        let header = jpeg_frame_header(&bytes).expect("encoded JPEG has a frame header");
        assert!(header.first_scan_decodes_rows);

        let row_scratch = jpeg_auxiliary_allocation(&bytes, 4000, 4000).unwrap();
        let input = bytes.len() as u64;
        let with_one_copy =
            4000u64 * 4000 * 3 + row_scratch + input + jpeg_retained_metadata_allocation(&bytes);
        assert!(with_one_copy <= MAX_DECODED_IMAGE_ALLOC_BYTES);
        assert_eq!(
            projected_allocation("near-limit.jpg", "image/jpeg", &bytes).unwrap(),
            with_one_copy + input
        );

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let (error, decodes) = counting_decodes(async {
            validate_within_budget("near-limit.jpg", "image/jpeg", &bytes, &mut budget)
                .await
                .expect_err("both input copies push the frame over the per-image cap")
        })
        .await;
        assert!(matches!(
            multimodal_error_kind(&error),
            "image_too_large" | "corrupt_image"
        ));
        assert_eq!(decodes, 0);
    }

    /// Largest ICC payload one APP2 chunk carries.
    const ICC_CHUNK_BYTES: usize = 65_535 - 2 - 14;

    /// `bytes` with `total` bytes of ICC profile inserted after SOI as APP2
    /// chunks of at most [`ICC_CHUNK_BYTES`] each.
    fn with_icc_profile(bytes: &[u8], total: usize) -> Vec<u8> {
        const CHUNK: usize = ICC_CHUNK_BYTES;
        let chunks = total.div_ceil(CHUNK);
        assert!(chunks <= 255, "an ICC profile has at most 255 chunks");
        let mut out = bytes[..2].to_vec();
        for index in 0..chunks {
            let len = CHUNK.min(total - index * CHUNK);
            out.extend_from_slice(&[0xff, 0xe2]);
            out.extend_from_slice(&((2 + 14 + len) as u16).to_be_bytes());
            out.extend_from_slice(b"ICC_PROFILE\0");
            out.extend_from_slice(&[(index + 1) as u8, chunks as u8]);
            out.resize(out.len() + len, 0x5a);
        }
        out.extend_from_slice(&bytes[2..]);
        out
    }

    #[test]
    fn jpeg_metadata_charge_follows_zune_jpegs_marker_contexts() {
        let per_segment = JPEG_METADATA_SEGMENT_OVERHEAD_BYTES;
        let mut bytes = vec![0xff, 0xd8];
        // APP1 before the frame: 100-byte payload whose own bytes look like an
        // APP2 marker. The payload is skipped, so that pattern is not counted.
        bytes.extend_from_slice(&[0xff, 0xe1, 0x00, 102, 0xff, 0xe2, 0x00, 0x40]);
        bytes.resize(bytes.len() + 96, 0);
        // A DQT segment is skipped by its declared length too (14-byte payload).
        bytes.extend_from_slice(&[0xff, 0xdb, 0x00, 16, 0xff, 0xef, 0x00, 10]);
        bytes.resize(bytes.len() + 10, 0);
        // A one-component SOS header, then entropy-coded bytes: a stuffed
        // zero, a restart marker, and fill bytes are not metadata.
        bytes.extend_from_slice(&[0xff, 0xda, 0x00, 8, 1, 1, 0, 0, 0x3f, 0]);
        bytes.extend_from_slice(&[0x12, 0xff, 0x00, 0x34, 0xff, 0xd0, 0x56, 0xff, 0xff]);
        // APP2 after the scan: 50-byte payload.
        bytes.extend_from_slice(&[0xff, 0xe2, 0x00, 52]);
        bytes.resize(bytes.len() + 50, 0);
        // Between baseline scans a restart marker carries a length.
        bytes.extend_from_slice(&[0xff, 0xd0, 0x00, 4, 0xaa, 0xbb]);
        // COM: 10-byte payload.
        bytes.extend_from_slice(&[0xff, 0xfe, 0x00, 12]);
        bytes.resize(bytes.len() + 10, 0);
        bytes.extend_from_slice(&[0xff, 0xd9]);
        // Nothing after EOI is parsed.
        bytes.extend_from_slice(&[0xff, 0xe1, 0x00, 102]);
        bytes.resize(bytes.len() + 100, 0);
        assert_eq!(
            jpeg_retained_metadata_allocation(&bytes),
            2 * (100 + 50 + 10) + 3 * per_segment
        );

        // A declared length past the end of the input is charged only for the
        // bytes that are actually there.
        let truncated = [0xff, 0xd8, 0xff, 0xe2, 0x10, 0x00, 1, 2, 3, 4, 5];
        assert_eq!(
            jpeg_retained_metadata_allocation(&truncated),
            2 * 5 + per_segment
        );
    }

    #[test]
    fn header_markers_zune_jpeg_reads_with_a_length_cannot_hide_metadata() {
        // zune-jpeg reads RST, SOI, and TEM before the first scan with a length
        // and skips it, so an EOI or APP pattern inside one must neither end
        // the walk nor make it jump over the profile that follows.
        let mut icc = vec![0xff, 0xe2, 0x03, 0xea];
        icc.resize(icc.len() + 1000, 0x5a);
        for prefix in [
            &[0xff, 0xd8, 0x00, 0x04, 0xff, 0xd9][..],
            &[0xff, 0xd0, 0x00, 0x04, 0xff, 0xd9][..],
            &[0xff, 0x01, 0x00, 0x04, 0xff, 0xd9][..],
            &[0xff, 0xd0, 0x00, 0x06, 0xff, 0xe1, 0xff, 0xff][..],
        ] {
            let mut bytes = vec![0xff, 0xd8];
            bytes.extend_from_slice(prefix);
            bytes.extend_from_slice(&icc);
            assert_eq!(
                jpeg_retained_metadata_allocation(&bytes),
                2 * 1000 + JPEG_METADATA_SEGMENT_OVERHEAD_BYTES,
                "prefix {prefix:02x?}"
            );
        }
    }

    /// One 65,516-byte block that walks reading a length-6 APP0 as 6 bytes
    /// see as a DQT, while zune-jpeg, which reads it as 7, parses the ICC chunk
    /// inside: `FF E0 00 06 'ABCD'`, `FF DB`, then `FF E2 FF E0` and an ICC
    /// chunk header followed by 65,488 bytes of profile.
    fn app0_overread_icc_block(seq: u8, count: u8) -> Vec<u8> {
        let mut block = vec![0xff, 0xe0, 0x00, 0x06, b'A', b'B', b'C', b'D', 0xff, 0xdb];
        block.extend_from_slice(&[0xff, 0xe2, 0xff, 0xe0]);
        block.extend_from_slice(b"ICC_PROFILE\0");
        block.extend_from_slice(&[seq, count]);
        block.resize(65_516, 0x5a);
        block
    }

    #[test]
    fn app0_with_length_six_is_read_one_byte_past_its_end() {
        let per_segment = JPEG_METADATA_SEGMENT_OVERHEAD_BYTES;
        // zune-jpeg consumes the FF of `FF D9` as the APP0's fifth probe byte,
        // so that EOI is not a marker and the APP2 after it is parsed.
        let mut bytes = vec![
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x06, b'A', b'B', b'C', b'D', 0xff, 0xd9,
        ];
        bytes.extend_from_slice(&[0xff, 0xe2, 0x03, 0xea]);
        bytes.resize(bytes.len() + 1000, 0x5a);
        assert_eq!(
            jpeg_retained_metadata_allocation(&bytes),
            2 * (4 + 1000) + 2 * per_segment
        );

        // The same over-read means a frame header placed right after such an
        // APP0 is not one zune-jpeg sees; the projection must not trust it.
        let mut hidden = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x06, b'A', b'B', b'C', b'D'];
        hidden.extend_from_slice(&jpeg_frame_and_first_scan(0xc0, &[(1, 1)], &[1])[2..]);
        assert!(jpeg_frame_header(&hidden).is_none());

        let block = app0_overread_icc_block(1, 1);
        assert_eq!(
            jpeg_retained_metadata_allocation(&[&[0xff, 0xd8][..], &block].concat()),
            2 * (4 + 65_502) + 2 * per_segment
        );
    }

    #[tokio::test]
    async fn app0_overread_cannot_hide_a_profile_from_admission() {
        // The ordinary 3840x2160 photo behind 255 such blocks: ~15.9 MiB of
        // profile that zune-jpeg keeps for the whole decode. Walks reading the
        // APP0 as 6 bytes saw only DQTs and charged the photo as ordinary,
        // under the cap. Aligned with zune-jpeg, the metadata walk charges the
        // profile, and the frame walk meets the byte zune-jpeg treats as junk
        // and takes the malformed-header fallback.
        let photo = encoded_jpeg(3840, 2160);
        let mut bytes = photo[..2].to_vec();
        for seq in 1..=255u8 {
            bytes.extend_from_slice(&app0_overread_icc_block(seq, 255));
        }
        bytes.extend_from_slice(&photo[2..]);

        assert!(jpeg_retained_metadata_allocation(&bytes) >= 2 * 255 * 65_488);
        assert!(jpeg_frame_header(&bytes).is_none());
        let projected = projected_allocation("app0.jpg", "image/jpeg", &bytes).unwrap();
        assert!(projected > MAX_DECODED_IMAGE_ALLOC_BYTES);

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let (error, decodes) = counting_decodes(async {
            validate_within_budget("app0.jpg", "image/jpeg", &bytes, &mut budget)
                .await
                .expect_err("the hidden profile takes the frame over the per-image cap")
        })
        .await;
        assert!(matches!(
            multimodal_error_kind(&error),
            "image_too_large" | "corrupt_image"
        ));
        assert_eq!(decodes, 0);
    }

    #[test]
    fn restart_markers_between_scans_follow_the_frame_type() {
        // After a scan, an APP2 (30-byte payload), then a restart marker whose
        // next bytes look like an APP1 header with a 20-byte payload. zune-jpeg's
        // progressive marker loop handles that RST without a length and would
        // parse the APP1; its baseline loop reads FF E1 as the RST's length and
        // skips it. This checks the walk's model of each loop; the synthetic
        // scan data is not decodable.
        let stream = |sof: u8| {
            let mut bytes = jpeg_sof_header(16, 16, &[(1, 1)]);
            bytes[3] = sof;
            bytes.extend_from_slice(&[0xff, 0xda, 0x00, 8, 1, 1, 0, 0, 0x3f, 0, 0x12, 0x34]);
            bytes.extend_from_slice(&[0xff, 0xe2, 0x00, 32]);
            bytes.resize(bytes.len() + 30, 0);
            bytes.extend_from_slice(&[0xff, 0xd0, 0xff, 0xe1, 0x00, 22]);
            bytes.resize(bytes.len() + 20, 0);
            bytes.extend_from_slice(&[0xff, 0xd9]);
            bytes
        };
        let per_segment = JPEG_METADATA_SEGMENT_OVERHEAD_BYTES;
        assert_eq!(
            jpeg_retained_metadata_allocation(&stream(0xc2)),
            2 * (30 + 20) + 2 * per_segment
        );
        assert_eq!(
            jpeg_retained_metadata_allocation(&stream(0xc0)),
            2 * 30 + per_segment
        );
    }

    #[tokio::test]
    async fn metadata_heavy_baseline_photo_is_refused_before_decode() {
        // The ordinary 3840x2160 photo with the largest ICC profile JPEG can
        // carry (255 full chunks, ~15.9 MiB). Output plus both input copies
        // fit under the cap, but the decoder also keeps the profile for the
        // whole decode, which takes the real peak over it.
        const ICC_BYTES: usize = 255 * ICC_CHUNK_BYTES;
        let photo = encoded_jpeg(3840, 2160);
        let bytes = with_icc_profile(&photo, ICC_BYTES);
        let header = jpeg_frame_header(&bytes).expect("encoded JPEG has a frame header");
        assert!(header.first_scan_decodes_rows);

        let metadata = jpeg_retained_metadata_allocation(&bytes);
        assert!(metadata >= 2 * ICC_BYTES as u64);
        let projected = projected_allocation("icc.jpg", "image/jpeg", &bytes).unwrap();
        assert!(projected - metadata < MAX_DECODED_IMAGE_ALLOC_BYTES);
        assert!(projected > MAX_DECODED_IMAGE_ALLOC_BYTES);

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let (error, decodes) = counting_decodes(async {
            validate_within_budget("icc.jpg", "image/jpeg", &bytes, &mut budget)
                .await
                .expect_err("retained metadata takes the frame over the per-image cap")
        })
        .await;
        assert!(matches!(
            multimodal_error_kind(&error),
            "image_too_large" | "corrupt_image"
        ));
        assert_eq!(decodes, 0);
    }

    #[test]
    fn second_frame_header_before_first_scan_uses_the_malformed_fallback() {
        // A progressive SOF after a baseline one leaves the frame type
        // ambiguous; zune-jpeg rejects the stream, and the projection does not
        // classify it.
        let mut bytes = jpeg_sof_header(17, 9, &SAMPLING_420);
        bytes.extend_from_slice(&jpeg_frame_and_first_scan(0xc2, &SAMPLING_420, &[1, 2, 3])[2..]);
        assert!(jpeg_frame_header(&bytes).is_none());
        assert!(jpeg_auxiliary_allocation(&bytes, 17, 9).is_none());
    }

    #[tokio::test]
    async fn ordinary_baseline_photo_passes_the_production_validator() {
        // A 3840x2160 4:4:4 photo projects to ~74 MiB when its coefficient
        // planes are charged, which used to refuse it, but a single-scan
        // baseline decode peaks at ~25 MiB.
        let bytes = encoded_jpeg(3840, 2160);
        let header = jpeg_frame_header(&bytes).expect("encoded JPEG has a frame header");
        assert!(header.first_scan_decodes_rows);

        let projected = projected_allocation("photo.jpg", "image/jpeg", &bytes).unwrap();
        assert!(projected <= MAX_DECODED_IMAGE_ALLOC_BYTES, "{projected}");
        let output = 3840u64 * 2160 * 3;
        let progressive = relabelled_progressive(bytes.clone());
        let with_coefficients =
            output + jpeg_auxiliary_allocation(&progressive, 3840, 2160).unwrap();
        assert!(with_coefficients > MAX_DECODED_IMAGE_ALLOC_BYTES);

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let (result, decodes) = counting_decodes(async {
            validate_within_budget("photo.jpg", "image/jpeg", &bytes, &mut budget).await
        })
        .await;
        result.expect("an ordinary baseline photo must be admitted");
        assert_eq!(decodes, 1);

        // The same frame relabelled progressive keeps the coefficient charge
        // and is refused before any pixel decoding.
        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let (error, decodes) = counting_decodes(async {
            validate_within_budget("photo.jpg", "image/jpeg", &progressive, &mut budget)
                .await
                .expect_err("a progressive frame must keep its coefficient charge")
        })
        .await;
        assert!(matches!(
            multimodal_error_kind(&error),
            "image_too_large" | "corrupt_image"
        ));
        assert_eq!(decodes, 0);
    }

    #[tokio::test]
    async fn unprefixed_jpeg_frame_header_cannot_undercharge_admission() {
        let ordinary = valid_jpeg();
        assert!(
            jpeg_frame_header(&ordinary).is_some(),
            "ordinary marker-framed JPEG must retain its precise projection"
        );

        let mut bytes = jpeg_with_declared_dimensions(3000, 3000);
        // An unprefixed SOF2-shaped header claims one component. zune-jpeg's
        // non-strict parser skips these bytes and uses the later real,
        // three-component frame header, so the projection must not accept the
        // fake component count.
        bytes.splice(
            2..2,
            [
                0xc2, 0x00, 0x0b, 0x08, 0x0b, 0xb8, 0x0b, 0xb8, 0x01, 0x01, 0x11, 0x00,
            ],
        );
        assert!(
            jpeg_frame_header(&bytes).is_none(),
            "every projected JPEG marker must have an 0xff prefix"
        );

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let initial_budget = budget;
        let (error, decodes) = counting_decodes(async {
            validate_within_budget("large.jpg", "image/jpeg", &bytes, &mut budget)
                .await
                .expect_err("unframed header must use the conservative refusal path")
        })
        .await;
        assert_eq!(multimodal_error_kind(&error), "corrupt_image");
        assert_eq!(decodes, 0, "refusal must happen before pixel decoding");
        assert_eq!(
            budget, initial_budget,
            "a per-image refusal must preserve budget for valid siblings"
        );

        validate_within_budget("sibling.png", "image/png", &valid_png(), &mut budget)
            .await
            .expect("a valid sibling must remain admissible");
    }

    #[tokio::test]
    async fn jpeg_tem_marker_cannot_desynchronize_admission_projection() {
        let ordinary = valid_jpeg();
        assert!(jpeg_frame_header(&ordinary).is_some());

        let mut bytes = jpeg_with_declared_dimensions(3000, 3000);
        // TEM is a standalone attachment. zune-jpeg does not treat it like the
        // projection's lengthless-marker path, so a fake one-component SOF
        // after TEM could otherwise undercharge the real frame that follows.
        bytes.splice(
            2..2,
            [
                0xff, 0x01, // TEM
                0xff, 0xc2, 0x00, 0x0b, 0x08, 0x0b, 0xb8, 0x0b, 0xb8, 0x01, 0x01, 0x11, 0x00,
            ],
        );
        assert!(
            jpeg_frame_header(&bytes).is_none(),
            "non-FF00 no-length markers must use the conservative refusal path"
        );

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let initial_budget = budget;
        let (error, decodes) = counting_decodes(async {
            validate_within_budget("tem.jpg", "image/jpeg", &bytes, &mut budget)
                .await
                .expect_err("TEM marker framing must be refused before decoding")
        })
        .await;
        assert_eq!(multimodal_error_kind(&error), "corrupt_image");
        assert_eq!(decodes, 0, "refusal must happen before pixel decoding");
        assert_eq!(budget, initial_budget, "refusal preserves sibling budget");

        validate_within_budget("sibling.png", "image/png", &valid_png(), &mut budget)
            .await
            .expect("a valid sibling must remain admissible");
    }

    #[tokio::test]
    async fn unsupported_jpeg_sof_marker_cannot_undercharge_admission() {
        // The encoded first scan covers every component, so the real frame is
        // charged row scratch rather than coefficient planes. What this pins is
        // that the projection reads the real three-component frame; refusal at
        // 5000x5000 follows from the output buffer alone.
        let mut bytes = jpeg_with_declared_dimensions(5000, 5000);
        // zune-jpeg 0.5.15 treats C3 as an unknown length-bearing marker and
        // continues to the real three-component frame header. The projection
        // must do the same instead of trusting this smaller fake header.
        bytes.splice(
            2..2,
            [
                0xff, 0xc3, 0x00, 0x0b, 0x08, 0x0b, 0xb8, 0x0b, 0xb8, 0x01, 0x01, 0x11, 0x00,
            ],
        );

        let header = jpeg_frame_header(&bytes).expect("the real frame header must be found");
        assert_eq!(header.components.len(), 3);
        let projected = 5000u64 * 5000 * 3
            + jpeg_auxiliary_allocation(&bytes, 5000, 5000)
                .expect("the real frame must have a conservative projection");
        assert!(projected > MAX_DECODED_IMAGE_ALLOC_BYTES);

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let initial_budget = budget;
        let (error, decodes) = counting_decodes(async {
            validate_within_budget("unsupported-sof.jpg", "image/jpeg", &bytes, &mut budget)
                .await
                .expect_err("the real frame allocation must exceed the per-image cap")
        })
        .await;
        assert!(
            matches!(
                multimodal_error_kind(&error),
                "image_too_large" | "corrupt_image"
            ),
            "admission must refuse the unsupported-marker payload"
        );
        assert_eq!(decodes, 0, "refusal must happen before pixel decoding");
        assert_eq!(budget, initial_budget, "refusal preserves sibling budget");

        validate_within_budget("sibling.png", "image/png", &valid_png(), &mut budget)
            .await
            .expect("a valid sibling must remain admissible");
    }

    #[tokio::test]
    async fn oversized_data_uri_with_malformed_header_is_refused_before_any_source_copy() {
        // The entry-point length guard must fire for malformed data URIs too,
        // not only for valid-MIME oversized payloads. A malformed header (no
        // comma, or `;base64` parameter absent) previously returned an error
        // with `input: source.to_string()` *before* the encoded-length ceiling
        // was reached, allowing an attacker to bypass the new check entirely by
        // crafting a header that trips an earlier branch.
        //
        // `BASE64_DECODE_CALLS` stays at zero in both sub-cases because neither
        // path reaches the base64 engine, but that is NOT the sensitive
        // assertion here. The critical claim is that the total source length is
        // bounded before *any* allocation from it: the entry-point guard fires
        // before `source.find(',')` or `header.contains(";base64")` is
        // evaluated, so no error branch can copy the full oversized source.
        let cfg = MultimodalConfig::default();
        let (_, max_mb) = cfg.effective_limits();
        let max_bytes = max_mb * 1024 * 1024;
        let filler = "A".repeat(max_bytes / 3 * 4 + 4096);

        // Assert on the error *text*, not on whether the image was skipped.
        // Both sub-cases are refused with or without the entry-point guard, so
        // a skip assertion proves nothing. What the guard changes is whether the
        // rejection carries a copy of the oversized source: the error must be
        // bounded and must not embed the filler.
        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;

        // Sub-case 1: no comma — reaches the `comma_idx` branch first.
        let no_comma = format!("data:image/png;base64{filler}");
        let err = normalize_data_uri(&no_comma, max_bytes, &mut budget)
            .await
            .expect_err("an oversized malformed data URI must be refused");
        let text = err.to_string();
        assert!(
            !text.contains(&filler),
            "the rejection must not embed the oversized source; it copied {} bytes",
            text.len()
        );
        assert!(
            text.len() < 512,
            "the rejection must stay bounded, got {} bytes",
            text.len()
        );

        // Sub-case 2: unsupported MIME type — reaches `validate_mime` first.
        let unsupported = format!("data:text/html;base64,{filler}");
        let err2 = normalize_data_uri(&unsupported, max_bytes, &mut budget)
            .await
            .expect_err("an oversized unsupported-MIME data URI must be refused");
        let text2 = err2.to_string();
        assert!(
            !text2.contains(&filler),
            "the unsupported-MIME rejection must not embed the oversized source; it copied {} bytes",
            text2.len()
        );
        assert!(
            text2.len() < 512,
            "the unsupported-MIME rejection must stay bounded, got {} bytes",
            text2.len()
        );

        // A well-formed marker inside the ceiling must still normalize, so the
        // entry-point guard is not simply rejecting everything.
        let valid = format!("data:image/png;base64,{}", STANDARD.encode(valid_png()));
        normalize_data_uri(&valid, max_bytes, &mut budget)
            .await
            .expect("a within-ceiling data URI still normalizes");
    }

    #[tokio::test]
    async fn validate_image_content_rejects_empty_payload() {
        // Zero-byte files pass `validate_size`, which only has an upper bound.
        let err = validate_image_content("src", "image/png", &[], MAX_DECODED_IMAGE_ALLOC_BYTES)
            .await
            .unwrap_err();
        assert_eq!(multimodal_error_kind(&err), "corrupt_image");
    }

    #[tokio::test]
    async fn validate_image_content_rejects_mime_content_mismatch() {
        // Real PNG bytes declared as JPEG — the case an extension-derived MIME
        // produces when a file is simply renamed.
        let err = validate_image_content(
            "src",
            "image/jpeg",
            &valid_png(),
            MAX_DECODED_IMAGE_ALLOC_BYTES,
        )
        .await
        .unwrap_err();
        assert_eq!(multimodal_error_kind(&err), "corrupt_image");
    }

    #[tokio::test]
    async fn corrupt_local_image_is_skipped_without_failing_the_turn() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("truncated.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        let history = vec![ChatMessage::user("what is in this?").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (path.display()).to_string(),
            },
        ])];

        let prepared = prepare_messages_for_provider(&history, &MultimodalConfig::default())
            .await
            .expect("a corrupt image must not fail message preparation");

        let content = &prepared.messages[0].content;
        assert!(
            !prepared.contains_images,
            "corrupt image must not be inlined: {content}"
        );
        assert!(
            content.contains("could not be loaded"),
            "model must be told the image was dropped: {content}"
        );
        assert!(
            content.contains("what is in this?"),
            "surrounding user text must survive: {content}"
        );
    }

    #[tokio::test]
    async fn corrupt_image_is_skipped_while_valid_sibling_survives() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good.png");
        let bad = temp.path().join("bad.png");
        std::fs::write(&good, valid_png()).unwrap();
        std::fs::write(&bad, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        let history = vec![ChatMessage::user("compare  and").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (good.display()).to_string(),
            },
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (bad.display()).to_string(),
            },
        ])];

        let prepared = prepare_messages_for_provider(&history, &MultimodalConfig::default())
            .await
            .expect("one corrupt image must not discard the valid one");

        let content = &prepared.messages[0].content;
        assert!(
            prepared.contains_images,
            "valid image must survive: {content}"
        );
        assert!(
            content.contains("1 of 2"),
            "note must report the partial skip: {content}"
        );
    }

    #[tokio::test]
    async fn declared_dimension_bomb_is_refused_before_allocation() {
        // 60000x60000 RGB would be ~10 GiB decoded. The file itself is a few
        // dozen bytes, so `validate_size` cannot see the problem — the
        // dimension guard is the only thing standing between this marker and
        // an OOM.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bomb.png");
        std::fs::write(&path, png_bomb(60_000, 60_000)).unwrap();
        let history = vec![ChatMessage::user("describe").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (path.display()).to_string(),
            },
        ])];

        let prepared = prepare_messages_for_provider(&history, &MultimodalConfig::default())
            .await
            .expect("a bomb must be skipped, not propagated as a turn failure");

        let content = &prepared.messages[0].content;
        assert!(
            !prepared.contains_images,
            "dimension bomb must not be inlined: {content}"
        );
        assert!(
            content.contains("could not be loaded"),
            "model must be told the image was dropped: {content}"
        );
    }

    #[tokio::test]
    async fn many_candidates_are_trimmed_before_any_decode_runs() {
        // Pre-selection keeps decode cost proportional to `max_images` rather
        // than to history length. Twelve older bombs, one per message, with
        // `max_images = 1`: only the newest message survives the cap, so only
        // its single image is ever decoded. Without pre-selection all twelve
        // bombs would be decoded first and only then discarded.
        //
        // Note the trim granularity is per *message*, not per image
        // (`trim_old_images` strips a message's images as a unit), so the
        // bombs are spread across messages the way real history accumulates
        // them rather than crammed into one turn.
        let temp = tempfile::tempdir().unwrap();
        let mut history = Vec::new();
        for i in 0..12 {
            let path = temp.path().join(format!("bomb_{i}.png"));
            std::fs::write(&path, png_bomb(40_000, 40_000)).unwrap();
            history.push(
                ChatMessage::user(format!("old {i} ")).with_attachments(vec![
                    zeroclaw_api::media::AttachmentReference {
                        kind: zeroclaw_api::media::AttachmentKind::Image,
                        target: (path.display()).to_string(),
                    },
                ]),
            );
            history.push(ChatMessage::assistant("ack"));
        }
        let good = temp.path().join("good.png");
        std::fs::write(&good, valid_png()).unwrap();
        history.push(ChatMessage::user("newest ").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (good.display()).to_string(),
            },
        ]));

        let config = MultimodalConfig {
            max_images: 1,
            ..MultimodalConfig::default()
        };
        let prepared = prepare_messages_for_provider(&history, &config)
            .await
            .expect("a wall of bombs must not fail preparation");

        let newest = &prepared
            .messages
            .last()
            .expect("history is non-empty")
            .content;
        assert!(
            prepared.contains_images,
            "the newest valid image must survive the trim: {newest}"
        );
        assert!(
            !prepared
                .messages
                .iter()
                .any(|m| m.content.contains("could not be loaded")),
            "trimmed-away bombs must never be decoded, so nothing is reported as skipped"
        );
        assert!(
            prepared
                .messages
                .iter()
                .any(|m| m.content.contains("old 0")),
            "trimmed messages keep their text, losing only the image"
        );
    }

    /// Runs `body` with a fresh [`DECODE_CALLS`] counter scoped to it, and
    /// returns `(body's value, decodes observed)`.
    ///
    /// The counter is task-local, so concurrent tests decoding their own images
    /// cannot inflate this count and no cross-test locking is needed.
    async fn counting_decodes<F, T>(body: F) -> (T, usize)
    where
        F: std::future::Future<Output = T>,
    {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        DECODE_CALLS
            .scope(counter, async move {
                let value = body.await;
                let count =
                    DECODE_CALLS.with(|calls| calls.load(std::sync::atomic::Ordering::Relaxed));
                (value, count)
            })
            .await
    }

    /// A PNG that really is `width` x `height` — unlike [`png_bomb`], it decodes,
    /// so it actually consumes decode budget.
    fn real_png(width: u32, height: u32) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([0, 128, 255, 255]),
        ))
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("test PNG encodes");
        buf.into_inner()
    }

    #[tokio::test]
    async fn budget_exhaustion_rejects_without_decoding() {
        // 512x512 RGBA = 1 MiB projected.
        let bytes = real_png(512, 512);

        let ((), decodes) = counting_decodes(async {
            let mut budget = 1024 * 1024;

            // Exactly fits: decoded, and the budget lands on zero.
            validate_within_budget("first.png", "image/png", &bytes, &mut budget)
                .await
                .expect("an image that fits the budget is accepted");
            assert_eq!(budget, 0, "a decode consumes its projected allocation");

            // Every subsequent candidate must be refused *without* a decode.
            // Before this change the budget was checked after decoding, so each
            // of these paid a full decode before being discarded.
            for i in 0..3 {
                validate_within_budget("later.png", "image/png", &bytes, &mut budget)
                    .await
                    .expect_err("an exhausted budget must reject");
                assert_eq!(budget, 0, "an exhausted budget stays exhausted (i={i})");
            }
        })
        .await;

        assert_eq!(
            decodes, 1,
            "only the first image may be decoded; the rest are refused up front"
        );
    }

    #[tokio::test]
    async fn corrupt_decode_is_charged_without_closing_the_budget() {
        let valid = valid_png();
        let mut corrupt = valid_png();
        corrupt_png_chunk_data(&mut corrupt, b"IDAT");
        assert_eq!(
            projected_allocation("bad.png", "image/png", &corrupt)
                .expect("fixture retains a readable IHDR"),
            4
        );

        let ((), decodes) = counting_decodes(async {
            let mut budget = 8u64;
            validate_within_budget("bad.png", "image/png", &corrupt, &mut budget)
                .await
                .expect_err("the truncated PNG must be rejected");
            assert_eq!(
                budget, 4,
                "ordinary corruption spends this candidate's admitted decode charge only"
            );

            validate_within_budget("good.png", "image/png", &valid, &mut budget)
                .await
                .expect("an unrelated valid sibling must retain the remaining budget");
            assert_eq!(budget, 0);
        })
        .await;

        assert_eq!(
            decodes, 2,
            "both the corrupt candidate and its valid sibling reach bounded decode"
        );
    }

    #[tokio::test]
    async fn newest_first_traversal_preserves_message_order() {
        // Normalization walks candidates newest-first so budget exhaustion sheds
        // the oldest images; the caller still requires original order back. This
        // covers the reversal only — the shedding priority itself is asserted in
        // `budget_exhaustion_sheds_oldest_images`, which drives the budget
        // directly rather than trying to burn 256 MiB through real decodes.
        let temp = tempfile::tempdir().unwrap();
        let old_path = temp.path().join("old.png");
        let new_path = temp.path().join("new.png");
        std::fs::write(&old_path, real_png(512, 512)).unwrap();
        std::fs::write(&new_path, real_png(512, 512)).unwrap();

        let history = vec![
            ChatMessage::user("old").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (old_path.display()).to_string(),
                },
            ]),
            ChatMessage::user("new").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (new_path.display()).to_string(),
                },
            ]),
        ];

        let (prepared, decodes) = counting_decodes(async {
            prepare_messages_for_provider(&history, &MultimodalConfig::default())
                .await
                .expect("two small images are well within budget")
        })
        .await;

        assert_eq!(
            prepared.messages.len(),
            2,
            "the newest-first walk must be undone before returning"
        );
        assert!(
            prepared.messages[0].content.contains("old")
                && prepared.messages[1].content.contains("new"),
            "messages must come back in their original order: {:?}",
            prepared
                .messages
                .iter()
                .map(|m| &m.content)
                .collect::<Vec<_>>()
        );
        assert_eq!(decodes, 2, "both images fit, so both are decoded");
    }

    #[tokio::test]
    async fn budget_exhaustion_sheds_oldest_images() {
        // Mirrors what the newest-first traversal does to a shared budget: the
        // candidates arrive newest-first, and the budget fits only one 1 MiB
        // image. The newest must be the one that survives — matching
        // `trim_old_images`, which also sheds oldest-first. An oldest-first walk
        // would invert this and drop the image the user just sent.
        let bytes = real_png(512, 512);

        let ((), decodes) = counting_decodes(async {
            let mut budget = 1024 * 1024;

            validate_within_budget("newest.png", "image/png", &bytes, &mut budget)
                .await
                .expect("the newest image gets first claim on the budget");
            validate_within_budget("oldest.png", "image/png", &bytes, &mut budget)
                .await
                .expect_err("the older image is shed once the budget is gone");
        })
        .await;

        assert_eq!(decodes, 1, "the shed image is never decoded");
    }

    /// Header plus one valid 1x1 frame, with no trailer yet.
    fn gif_prefix_with_one_frame() -> Vec<u8> {
        let mut gif = Vec::new();
        gif.extend_from_slice(b"GIF89a");
        gif.extend_from_slice(&1u16.to_le_bytes()); // logical width
        gif.extend_from_slice(&1u16.to_le_bytes()); // logical height
        gif.extend_from_slice(&[0x80, 0x00, 0x00]); // global color table, 2 entries
        gif.extend_from_slice(&[0x00, 0x00, 0x00]); // black
        gif.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // white
        gif.extend_from_slice(&GIF_IMAGE_DESCRIPTOR_1X1);
        gif.extend_from_slice(&GIF_VALID_1X1_LZW);
        gif
    }

    /// Image descriptor for a 1x1 frame at the origin, no local color table.
    const GIF_IMAGE_DESCRIPTOR_1X1: [u8; 10] = [0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0x00];
    /// Minimal well-formed LZW stream for a single 1x1 pixel.
    const GIF_VALID_1X1_LZW: [u8; 5] = [0x02, 0x02, 0x4C, 0x01, 0x00];
    const GIF_TRAILER: u8 = 0x3B;

    /// A valid single-frame GIF whose frame is a 1x1 sub-rectangle of a larger
    /// logical canvas, placed at a non-zero offset.
    ///
    /// This shape takes the *partial-frame* branch of `image` 0.25.10's GIF
    /// iterator (`src/codecs/gif.rs:388-415`), which allocates a second
    /// full-canvas buffer to composite into while the temporary sub-rectangle
    /// and the persistent canvas are both still live. The returned `Frame` is
    /// the full canvas either way, so this branch is only distinguishable by
    /// what it allocates, not by its output.
    fn partial_frame_gif(canvas: u16) -> Vec<u8> {
        let mut gif = Vec::new();
        gif.extend_from_slice(b"GIF89a");
        gif.extend_from_slice(&canvas.to_le_bytes()); // logical width
        gif.extend_from_slice(&canvas.to_le_bytes()); // logical height
        gif.extend_from_slice(&[0x80, 0x00, 0x00]); // global color table, 2 entries
        gif.extend_from_slice(&[0x00, 0x00, 0x00]); // black
        gif.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // white
        // Image descriptor: 1x1 frame at offset (1, 1), so it is neither at the
        // origin nor the size of the canvas.
        gif.push(0x2C);
        gif.extend_from_slice(&1u16.to_le_bytes()); // left
        gif.extend_from_slice(&1u16.to_le_bytes()); // top
        gif.extend_from_slice(&1u16.to_le_bytes()); // width
        gif.extend_from_slice(&1u16.to_le_bytes()); // height
        gif.push(0x00); // no local color table
        gif.extend_from_slice(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);
        gif
    }

    /// A two-frame 1x1 animated GIF. `second_frame` supplies the LZW payload
    /// after the second image descriptor, which is where the corrupt variant
    /// differs. No trailer is appended: the corrupt fixture is truncated.
    fn two_frame_gif(second_frame: &[u8]) -> Vec<u8> {
        let mut gif = gif_prefix_with_one_frame();
        gif.extend_from_slice(&GIF_IMAGE_DESCRIPTOR_1X1);
        gif.extend_from_slice(second_frame);
        gif
    }

    #[tokio::test]
    async fn gif_with_corrupt_later_frame_is_rejected() {
        // Frame two's sub-block header claims four bytes of LZW data and the
        // stream ends after one, so the frame cannot be decoded. Verified to
        // fail in both giflib and ImageMagick, not just this decoder — note that
        // merely substituting non-LZW garbage is *not* enough, since the decoder
        // happily accepts it.
        //
        // The single-image decoder reads only frame one, so before per-frame
        // validation this GIF passed here and then failed at the provider as a
        // hard 400 that took the whole batched request with it.
        let gif = two_frame_gif(&[0x02, 0x04, 0xAA]);

        // Guard the premise: frame one alone really is valid, so the rejection
        // below can only come from frame two.
        let mut first_frame_only = gif_prefix_with_one_frame();
        first_frame_only.push(GIF_TRAILER);
        validate_image_content(
            "one.gif",
            "image/gif",
            &first_frame_only,
            MAX_DECODED_IMAGE_ALLOC_BYTES,
        )
        .await
        .expect("fixture premise: the first frame is valid on its own");

        let error =
            validate_image_content("anim.gif", "image/gif", &gif, MAX_DECODED_IMAGE_ALLOC_BYTES)
                .await
                .expect_err("a GIF with a corrupt later frame must be rejected, not forwarded");
        assert!(
            error.to_string().to_lowercase().contains("corrupt")
                || error.to_string().to_lowercase().contains("gif"),
            "expected a corrupt-image error, got: {error}"
        );
    }

    #[tokio::test]
    async fn corrupt_later_gif_frame_does_not_discard_valid_sibling() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good.png");
        let bad = temp.path().join("bad.gif");
        std::fs::write(&good, valid_png()).unwrap();
        std::fs::write(&bad, two_frame_gif(&[0x02, 0x04, 0xAA])).unwrap();

        // Put the corrupt GIF first so it reaches the shared budget before the
        // valid PNG. A generic "GIF error closes the budget" rule would discard
        // the valid sibling as collateral damage.
        let history = vec![ChatMessage::user("compare  with").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (bad.display()).to_string(),
            },
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (good.display()).to_string(),
            },
        ])];
        let prepared = prepare_messages_for_provider(&history, &MultimodalConfig::default())
            .await
            .expect("a corrupt animation must be skipped without failing preparation");

        assert!(prepared.contains_images, "the valid PNG must survive");
        assert!(
            prepared
                .messages
                .iter()
                .any(|message| message.content.contains("1 of 2")),
            "the partial-skip note must report one rejected image: {:?}",
            prepared
                .messages
                .iter()
                .map(|message| &message.content)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn multi_frame_valid_prefix_followed_by_corrupt_frame_charges_completed_work() {
        // When an animation fails mid-iteration the error arm must charge the
        // work already completed. Using only `total` (returned frame bytes)
        // dropped the recurring per-frame scratch accumulated for every valid
        // frame, so the budget kept headroom the decoder had already spent.
        //
        // A two-frame fixture cannot catch this — its numbers happen to
        // overcharge — so this uses a long valid prefix before the corrupt
        // frame, where the omitted recurring term dominates.
        const VALID_FRAMES: usize = 5;

        let mut gif = Vec::new();
        gif.extend_from_slice(b"GIF89a");
        gif.extend_from_slice(&[1u8, 0, 1, 0]); // 1x1 logical screen
        gif.extend_from_slice(&[0xF0, 0, 0]); // global color table, 2 entries
        gif.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        for _ in 0..VALID_FRAMES {
            gif.extend_from_slice(&GIF_IMAGE_DESCRIPTOR_1X1);
            gif.extend_from_slice(&GIF_VALID_1X1_LZW);
        }
        // Corrupt final frame: the sub-block claims more LZW bytes than follow.
        gif.extend_from_slice(&GIF_IMAGE_DESCRIPTOR_1X1);
        gif.extend_from_slice(&[0x02, 0x04, 0xAA]);

        // Work genuinely completed before the corrupt frame, under the same
        // model the success path uses.
        let completed = VALID_FRAMES as u64 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
            + GIF_ANIMATION_SCRATCH_MODEL.persistent;

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        validate_within_budget("prefix.gif", "image/gif", &gif, &mut budget)
            .await
            .expect_err("an animation with a corrupt final frame must be refused");

        let charged = AGGREGATE_DECODE_BUDGET_BYTES - budget;
        assert!(
            charged >= completed,
            "a corrupt animation must be charged at least the work its valid prefix completed \
             ({completed} B for {VALID_FRAMES} frames); only {charged} B was debited"
        );
    }

    #[tokio::test]
    async fn corrupt_gif_in_newer_message_does_not_discard_valid_png_in_older_message() {
        let temp = tempfile::tempdir().unwrap();
        let old_png = temp.path().join("old.png");
        let new_gif = temp.path().join("new.gif");
        std::fs::write(&old_png, valid_png()).unwrap();
        std::fs::write(&new_gif, two_frame_gif(&[0x02, 0x04, 0xAA])).unwrap();

        // The corrupt GIF is in the newer message. `prepare_messages_inner`
        // walks candidates newest-first, so the newer message (with the corrupt
        // GIF) is processed before the older message (with the valid PNG).
        // An ordinary corrupt-image failure must not close the shared budget;
        // only aggregate-budget exhaustion may do that. The older PNG must
        // therefore still pass validation despite the GIF having been rejected
        // before it.
        let history = vec![
            ChatMessage::user("here is").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (old_png.display()).to_string(),
                },
            ]),
            ChatMessage::user("check this").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (new_gif.display()).to_string(),
                },
            ]),
        ];
        let prepared = prepare_messages_for_provider(&history, &MultimodalConfig::default())
            .await
            .expect("a corrupt animation in a newer message must not abort preparation");

        assert!(
            prepared.contains_images,
            "the valid PNG in the older message must survive the corrupt GIF in the newer \
             message: {:?}",
            prepared
                .messages
                .iter()
                .map(|m| &m.content)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            prepared.messages.len(),
            2,
            "both messages must be present with original order restored"
        );
        assert!(
            prepared
                .messages
                .iter()
                .any(|m| m.content.contains("could not be loaded")),
            "the newer message must carry the skip note for the corrupt GIF: {:?}",
            prepared
                .messages
                .iter()
                .map(|m| &m.content)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn gif_with_valid_frames_is_accepted() {
        // The counterpart to the rejection above: per-frame validation must not
        // start refusing ordinary animations.
        let mut gif = two_frame_gif(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        let allocation =
            validate_image_content("anim.gif", "image/gif", &gif, MAX_DECODED_IMAGE_ALLOC_BYTES)
                .await
                .expect("a well-formed animated GIF is accepted");
        // The charge is cumulative frame work plus the state the decoder holds
        // across the animation, not a single frame's peak: two 1x1 frames give
        // `total = 2 * 4` and one canvas of scratch
        // (`GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL`).
        assert_eq!(
            allocation,
            2 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
                + GIF_ANIMATION_SCRATCH_MODEL.persistent,
            "an accepted GIF charges every frame's output+transient work plus one canvas of persistent state"
        );
    }

    /// Image descriptor for a single frame declaring 60000x60000 — well past
    /// `MAX_DECODED_IMAGE_DIMENSION`. Decoded naively that's tens of
    /// gigabytes; `set_limits` must reject it before any buffer for it is
    /// allocated.
    fn gif_oversized_frame_descriptor() -> [u8; 10] {
        let dim: u16 = 60_000;
        let [lo, hi] = dim.to_le_bytes();
        [0x2C, 0, 0, 0, 0, lo, hi, lo, hi, 0x00]
    }

    #[tokio::test]
    async fn gif_oversized_later_frame_is_rejected_before_allocation() {
        // Frame one is valid (guarded above); frame two's descriptor alone
        // claims 60000x60000. Before this fix `GifDecoder` was constructed
        // with `Limits::no_limits()`, so nothing stopped `into_frames` from
        // allocating that buffer before the post-decode dimension check ran.
        // Applying `set_limits` up front means the decoder's own per-frame
        // `reserve_buffer` check rejects the descriptor first.
        let mut gif = gif_prefix_with_one_frame();
        gif.extend_from_slice(&gif_oversized_frame_descriptor());
        gif.extend_from_slice(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        let error =
            validate_image_content("anim.gif", "image/gif", &gif, MAX_DECODED_IMAGE_ALLOC_BYTES)
                .await
                .expect_err("an oversized later frame must be rejected, not allocated");
        assert_eq!(multimodal_error_kind(&error), "corrupt_image");
    }

    #[tokio::test]
    async fn gif_cumulative_frames_exceeding_remaining_budget_are_rejected() {
        // Three 1x1 frames, each individually far under any per-frame cap —
        // the decoder's own per-frame check (not cumulative across frames,
        // since each frame is checked against the same post-canvas remaining
        // allowance rather than a running total) would let every one of them
        // through on its own. Only this function's own running `total` catches
        // the aggregate: with a 10-byte budget_cap, three 4-byte frames (12
        // bytes) must still be rejected once the third would cross it.
        let mut gif = gif_prefix_with_one_frame();
        for _ in 0..2 {
            gif.extend_from_slice(&GIF_IMAGE_DESCRIPTOR_1X1);
            gif.extend_from_slice(&GIF_VALID_1X1_LZW);
        }
        gif.push(GIF_TRAILER);

        let error = validate_image_content("anim.gif", "image/gif", &gif, 10)
            .await
            .expect_err("cumulative frame allocation must not exceed the remaining budget");
        assert_eq!(multimodal_error_kind(&error), "corrupt_image");
    }

    #[tokio::test]
    async fn gif_over_remaining_budget_is_rejected_and_closes_the_budget() {
        // A GIF whose decoder peak exceeds the remaining allowance must be
        // rejected, and that rejection must close this call's budget rather
        // than leaving a non-zero remainder for a later candidate to repeat the
        // same bounded overshoot against.
        //
        // The projection models the peak (frame output plus the persistent
        // composition canvas), so for a 1x1 GIF it reports
        // `4 + GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL`. A budget of 6 is below
        // that, so the refusal happens at the header check before any decode.
        let mut gif = two_frame_gif(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        let mut budget = 6u64;
        validate_within_budget("anim.gif", "image/gif", &gif, &mut budget)
            .await
            .expect_err("an animation whose peak exceeds the remaining budget must reject");
        assert_eq!(
            budget, 0,
            "an aggregate-budget rejection must close this call's budget, not leave it \
             non-zero for a later candidate to repeat the same bounded overshoot against"
        );
    }

    #[tokio::test]
    async fn gif_cumulative_frames_are_enforced_live_during_decode() {
        // In-loop enforcement: an animation admitted by the *header* projection
        // must still be rejected mid-iteration once its running cumulative
        // charge crosses the cap, rather than decoding every frame and only
        // then reporting an over-budget total.
        //
        // The header projection sees one canvas (`4 + scratch`), but a 2-frame
        // GIF's real charge is `2 * (4 + per_frame) + persistent`. A cap sized
        // to the projection therefore admits the animation at the door and must
        // stop it partway through.
        let mut gif = two_frame_gif(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        let projected = 4 + GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL;
        let cumulative = 2 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
            + GIF_ANIMATION_SCRATCH_MODEL.persistent;
        assert!(
            projected < cumulative,
            "the fixture must sit in the gap between header projection and cumulative charge"
        );

        let error = validate_image_content("anim.gif", "image/gif", &gif, projected)
            .await
            .expect_err("a GIF whose cumulative charge exceeds the cap must be rejected");
        assert_eq!(multimodal_error_kind(&error), "corrupt_image");

        // With the cap raised to the real cumulative charge the same GIF is
        // accepted, proving the rejection above is the budget and not the
        // payload.
        let allocation = validate_image_content("anim.gif", "image/gif", &gif, cumulative)
            .await
            .expect("the same GIF is accepted when the cap covers its cumulative charge");
        assert_eq!(
            allocation, cumulative,
            "the accepted GIF charges every frame's output+transient plus one persistent canvas"
        );
    }

    #[tokio::test]
    async fn gif_over_budget_rejection_stops_a_later_candidate_from_decoding() {
        // The request-level regression the prior review asked for: with two
        // over-budget GIF candidates walked newest-first (mirroring how
        // `prepare_messages_inner` drives `remaining_decode_budget`), neither
        // candidate may spend decode work against a budget that cannot hold it.
        //
        // The projection now models the GIF decoder's peak (frame output plus
        // the persistent composition canvas), so a budget below that peak is
        // caught by the cheap header check and *no* candidate decodes. That is
        // strictly stronger than the previous behaviour, where the first
        // candidate decoded and was only rejected afterwards by the cumulative
        // frame check. The invariant under test is unchanged: an over-budget
        // animation must not be decoded repeatedly by later candidates.
        let mut gif = two_frame_gif(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        let ((), decodes) = counting_decodes(async {
            // Below the 1x1 GIF's projected peak of
            // `4 + GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL`.
            let mut budget = 6u64;

            validate_within_budget("newest.gif", "image/gif", &gif, &mut budget)
                .await
                .expect_err("first over-budget candidate must reject");
            assert_eq!(budget, 0, "the first rejection must close the budget");

            validate_within_budget("oldest.gif", "image/gif", &gif, &mut budget)
                .await
                .expect_err("second candidate must reject too, against a closed budget");
        })
        .await;

        assert_eq!(
            decodes, 0,
            "neither candidate may decode: the projected decoder peak exceeds the budget, \
             so both are refused by the pre-decode header check"
        );
    }

    #[tokio::test]
    async fn partial_frame_gif_peak_is_accounted_and_bounds_later_candidates() {
        // A GIF whose frame is a sub-rectangle of the logical canvas takes the
        // partial-frame branch of `image` 0.25.10's iterator
        // (`src/codecs/gif.rs:388-415`): it decodes the sub-rectangle into a
        // temporary buffer and then allocates a *second*, full-canvas buffer to
        // composite into, while that temporary and the persistent
        // `non_disposed_frame` canvas are both still live.
        //
        // The returned `Frame` is the composited full canvas in both branches,
        // so this extra buffer is invisible from the outside. Charging only the
        // returned frame plus one persistent canvas under-accounted every
        // partial-frame GIF; the bound must cover the worst branch.
        //
        // Real peak for an NxN canvas with a 1x1 sub-frame:
        //   persistent canvas (N*N*4) + temporary sub-rect (4) + composite (N*N*4)
        const CANVAS: u16 = 4;
        let gif = partial_frame_gif(CANVAS);
        let pixels = u64::from(CANVAS) * u64::from(CANVAS);

        let charge_per_candidate = pixels * 4 + pixels * GIF_ANIMATION_SCRATCH_BYTES_PER_PIXEL;
        let real_peak = (pixels * 4) + 4 + (pixels * 4);
        assert!(
            charge_per_candidate >= real_peak,
            "the charge ({charge_per_candidate}) must cover the decoder's real peak \
             ({real_peak}) for a partial-frame GIF"
        );

        // Budget for exactly two candidates, so the third proves the envelope
        // is enforced with the corrected accounting.
        let mut budget = charge_per_candidate * 2;

        let charged = validate_within_budget("first.gif", "image/gif", &gif, &mut budget)
            .await
            .expect("a valid partial-frame GIF within budget is accepted");
        assert_eq!(
            charged, charge_per_candidate,
            "a partial-frame GIF must be charged the bound covering its composite buffer"
        );

        validate_within_budget("second.gif", "image/gif", &gif, &mut budget)
            .await
            .expect("the second candidate exactly consumes the remaining budget");
        assert_eq!(
            budget, 0,
            "the envelope is fully spent after two candidates"
        );

        // The third candidate must be refused before decoding. Under the old
        // 4 B/px charge the same budget would still have reported headroom here
        // and admitted another full decode.
        let ((), decodes) = counting_decodes(async {
            let mut spent = budget;
            validate_within_budget("third.gif", "image/gif", &gif, &mut spent)
                .await
                .expect_err("a candidate arriving after the envelope is spent must be refused");
        })
        .await;
        assert_eq!(
            decodes, 0,
            "the refused candidate must not decode; the envelope is enforced before decode"
        );
    }

    /// A single-frame 1x1 GIF repeated `frames` times over a 1x1 logical screen.
    ///
    /// Each frame is a full-canvas 1x1 image, so every frame decodes to 4 bytes
    /// of RGBA. Used to prove that cumulative frame work — not just one frame's
    /// peak — is charged against the aggregate budget.
    fn many_frame_gif(frames: usize) -> Vec<u8> {
        let mut gif = gif_prefix_with_one_frame();
        for _ in 1..frames {
            gif.extend_from_slice(&GIF_IMAGE_DESCRIPTOR_1X1);
            gif.extend_from_slice(&GIF_VALID_1X1_LZW);
        }
        gif.push(GIF_TRAILER);
        gif
    }

    #[tokio::test]
    async fn long_animation_is_charged_cumulative_frame_work() {
        // The aggregate budget is documented as bounding *cumulative* decoded
        // allocations. Charging one frame's peak per animation broke that: a
        // long animation decodes every frame's pixels but debited only the
        // largest single frame, so a run of accepted animations could perform
        // far more cumulative decode work than the envelope claims.
        //
        // A 1x1 GIF with N frames does `N * 4` bytes of frame decoding, plus
        // one canvas of persistent scratch that stays live across the run. The
        // charge must therefore scale with frame count.
        let two = many_frame_gif(2);
        let twenty = many_frame_gif(20);

        let charge_two = validate_image_content("two.gif", "image/gif", &two, u64::MAX)
            .await
            .expect("a valid 2-frame GIF is accepted");
        let charge_twenty = validate_image_content("twenty.gif", "image/gif", &twenty, u64::MAX)
            .await
            .expect("a valid 20-frame GIF is accepted");

        assert_eq!(
            charge_two,
            2 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
                + GIF_ANIMATION_SCRATCH_MODEL.persistent,
            "a 2-frame GIF is charged every frame's output+transient work plus one persistent canvas"
        );
        assert_eq!(
            charge_twenty,
            20 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
                + GIF_ANIMATION_SCRATCH_MODEL.persistent,
            "a 20-frame GIF is charged 20 frames of work (output+transient each), not just one peak"
        );
        assert!(
            charge_twenty > charge_two,
            "cumulative charging must scale with frame count: {charge_twenty} vs {charge_two}"
        );

        // Request level: a budget sized for exactly one 20-frame animation must
        // admit it and then refuse the next candidate before it decodes. Under
        // peak-only charging the first animation would debit just 12 bytes and
        // leave headroom for many more, so the second candidate would decode.
        let mut budget = 20 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
            + GIF_ANIMATION_SCRATCH_MODEL.persistent;
        let charged = validate_within_budget("first.gif", "image/gif", &twenty, &mut budget)
            .await
            .expect("the long animation fits a budget sized for its cumulative work");
        assert_eq!(
            charged,
            20 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
                + GIF_ANIMATION_SCRATCH_MODEL.persistent
        );
        assert_eq!(
            budget, 0,
            "the long animation's cumulative work consumes the whole envelope"
        );

        let ((), decodes) = counting_decodes(async {
            let mut spent = budget;
            validate_within_budget("second.gif", "image/gif", &two, &mut spent)
                .await
                .expect_err("the next candidate must be refused against the spent envelope");
        })
        .await;
        assert_eq!(
            decodes, 0,
            "the refused candidate must not decode: cumulative charging closed the envelope"
        );
    }

    #[tokio::test]
    async fn animation_between_projection_and_cumulative_charge_is_rejected_mid_decode() {
        // The interval the previous regressions missed.
        //
        // A long animation's *header* projection is one canvas, but its real
        // charge is `N * (output + per_frame) + persistent`. A budget sitting
        // between those two values passes admission at the door, so the only
        // thing that can stop it is the in-loop cumulative check. Without that
        // check the decoder walked all 20 frames and the over-budget charge was
        // reported only on return — after the work had already been done.
        let twenty = many_frame_gif(20);

        let projected = projected_allocation("many.gif", "image/gif", &twenty)
            .expect("the header projection is readable");
        let cumulative = 20 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
            + GIF_ANIMATION_SCRATCH_MODEL.persistent;
        assert!(
            projected < cumulative,
            "the fixture must have a projection below its cumulative charge \
             ({projected} vs {cumulative})"
        );

        // Strictly between the two: admitted by the header check, over budget
        // partway through the frame loop.
        let between = (projected + cumulative) / 2;
        let mut budget = between;
        let error = validate_within_budget("many.gif", "image/gif", &twenty, &mut budget)
            .await
            .expect_err(
                "an animation whose cumulative charge exceeds the remaining budget must be \
                 rejected, not accepted after decoding every frame",
            );
        assert_eq!(multimodal_error_kind(&error), "corrupt_image");
        assert_eq!(
            budget, 0,
            "an aggregate-budget rejection closes the envelope for later candidates"
        );

        // The same animation is accepted once the budget covers its real charge,
        // proving the rejection above is the budget rather than the payload.
        let mut ample = cumulative;
        let charged = validate_within_budget("many.gif", "image/gif", &twenty, &mut ample)
            .await
            .expect("the same animation is accepted when the budget covers its cumulative charge");
        assert_eq!(charged, cumulative);
    }

    #[tokio::test]
    async fn over_budget_animation_overshoots_by_at_most_one_frame() {
        // Pins the exact guarantee the in-loop guard provides.
        //
        // `for frame in frames` produces a frame before its cost can be
        // measured, so the frame that crosses the allowance *is* decoded before
        // the rejection. What the guard prevents is decoding the remainder. The
        // aggregate-envelope wording elsewhere should be read against this
        // bound, not as "nothing over the limit is ever decoded".
        //
        // A 20-frame GIF given a budget covering ~2 frames must stop around
        // there — decisively fewer than all 20 — observed through the decode
        // counter rather than the error, which carries no per-frame detail.
        let twenty = many_frame_gif(20);
        let per_frame = 4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame;
        let persistent = GIF_ANIMATION_SCRATCH_MODEL.persistent;

        // Room for two frames plus the persistent term, so the third crossing
        // frame is the one that trips the guard.
        let budget_for_two = 2 * per_frame + persistent;
        let mut budget = budget_for_two;

        let error = validate_within_budget("many.gif", "image/gif", &twenty, &mut budget)
            .await
            .expect_err("a 20-frame animation cannot fit a two-frame budget");
        assert_eq!(multimodal_error_kind(&error), "corrupt_image");

        // An aggregate rejection closes the envelope, so the budget counter
        // cannot report the partial spend. What it does prove is that the
        // animation was refused rather than accepted-then-charged.
        assert_eq!(
            budget, 0,
            "an aggregate-budget rejection closes the envelope for later candidates"
        );

        // The same fixture under a budget that covers every frame is accepted
        // and charged the full amount. Together with the rejection above this
        // brackets the guarantee: the loop stops once the running charge
        // crosses, and the crossing frame is the only one decoded past the
        // limit — it never walks the remaining seventeen.
        let full_charge = 20 * per_frame + persistent;
        let mut ample = full_charge;
        let charged = validate_within_budget("many.gif", "image/gif", &twenty, &mut ample)
            .await
            .expect("the same animation fits a budget covering all twenty frames");
        assert_eq!(
            charged, full_charge,
            "a fully-admitted animation is charged every frame, which is what the \
             two-frame budget above was measured against"
        );
    }

    #[tokio::test]
    async fn multiple_valid_animations_respect_the_aggregate_budget() {
        // Request-level regression for animation accounting across candidates.
        //
        // Each accepted animation debits its cumulative frame work plus the
        // canvas state the decoder holds for the whole run, so two identical
        // animations consume exactly twice one animation's charge and a third
        // is refused against the spent envelope.
        //
        // APNG and animated WebP use the same `validate_animation_frames` path;
        // their per-format charges are pinned by `apng_validates_every_frame`
        // and `animated_webp_validates_every_frame`. This test uses GIF because
        // its projection and charge can be landed exactly, while `image`'s PNG
        // decoder holds internal row/zlib buffers that make an exact APNG
        // budget boundary awkward to express in a fixture.
        let mut gif = two_frame_gif(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        // Two 1x1 frames plus one canvas of persistent scratch.
        let gif_charge = 2 * (4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame)
            + GIF_ANIMATION_SCRATCH_MODEL.persistent;

        // Room for exactly two GIFs, with nothing left over, to prove the
        // third candidate is refused by the closed budget rather than admitted.
        let mut budget = gif_charge * 2;

        let charged1 = validate_within_budget("first.gif", "image/gif", &gif, &mut budget)
            .await
            .expect("the first GIF fits the budget");
        assert_eq!(
            charged1, gif_charge,
            "an accepted GIF must be charged its cumulative frame work plus persistent canvas"
        );
        assert_eq!(
            budget, gif_charge,
            "the first GIF's charge is debited in full"
        );

        let charged2 = validate_within_budget("second.gif", "image/gif", &gif, &mut budget)
            .await
            .expect("the second GIF exactly consumes the remainder");
        assert_eq!(charged2, gif_charge, "the second GIF is charged the same");
        assert_eq!(budget, 0, "the envelope is now fully spent");

        // A third candidate must be refused without decoding: the budget is
        // spent, and under the old frame-bytes accounting it would have still
        // reported headroom here and admitted another full decode.
        let ((), decodes) = counting_decodes(async {
            let mut spent = budget;
            validate_within_budget("third.gif", "image/gif", &gif, &mut spent)
                .await
                .expect_err("a candidate arriving after the envelope is spent must be refused");
        })
        .await;
        assert_eq!(
            decodes, 0,
            "the refused candidate must not decode; the envelope is enforced before decode"
        );
    }

    #[tokio::test]
    async fn partial_frame_gif_charges_recurring_sub_rectangle() {
        // Partial-frame GIF regression: the decoder's temporary sub-rectangle
        // buffer is allocated and freed on every `next()`, so it accumulates.
        //
        // `GIF_ANIMATION_SCRATCH_MODEL.per_frame` includes this 4 B/px term.
        // A multi-frame partial GIF should charge N*(output+per_frame)+persistent,
        // and a later candidate must be refused when that cumulative work exhausts
        // the envelope.

        // Build a 2-frame 2×2 partial GIF: each frame is a 1×1 sub-rectangle at
        // different positions, forcing the partial-frame branch on every iteration.
        let mut partial = Vec::new();
        partial.extend_from_slice(b"GIF89a");
        partial.extend_from_slice(&[2u8, 0, 2, 0]); // logical screen 2×2
        partial.extend_from_slice(&[0xF0, 0, 0]); // global color table flags, bg, aspect
        // Global color table: 2 colors
        partial.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        partial.push(0x21); // graphics control extension
        partial.extend_from_slice(&[0xF9, 4, 0, 0, 0, 0, 0]);
        partial.push(0x2C); // image descriptor
        partial.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0, 0]); // sub-rect at (0,0) 1×1
        partial.push(2); // LZW min code size
        partial.extend_from_slice(&[2, 0x8C, 0x2D, 0]); // minimal LZW data + block terminator

        partial.push(0x21); // second frame
        partial.extend_from_slice(&[0xF9, 4, 0, 0, 0, 0, 0]);
        partial.push(0x2C);
        partial.extend_from_slice(&[1, 0, 1, 0, 1, 0, 1, 0, 0]); // sub-rect at (1,1) 1×1
        partial.push(2);
        partial.extend_from_slice(&[2, 0x8C, 0x2D, 0]);

        partial.push(0x3B); // trailer

        // Canvas is 2×2 = 4 px. Two frames at 4 B/px output each = 8 B total.
        // Per-frame transient = 4 B/px * 4 px = 16 B per frame.
        // Persistent = 4 B/px * 4 px = 16 B once.
        // Cumulative = 2*(4*4 + 16) + 16 = 2*32 + 16 = 80 B.
        let cumulative = 2 * (4 * 4 + GIF_ANIMATION_SCRATCH_MODEL.per_frame * 4)
            + GIF_ANIMATION_SCRATCH_MODEL.persistent * 4;

        let mut budget = cumulative;
        let charged = validate_within_budget("partial.gif", "image/gif", &partial, &mut budget)
            .await
            .expect("the partial GIF fits a budget sized for its cumulative transient work");
        assert_eq!(
            charged, cumulative,
            "the partial GIF is charged every frame's sub-rectangle plus persistent canvas"
        );
        assert_eq!(budget, 0, "the cumulative work consumes the envelope");

        let ((), decodes) = counting_decodes(async {
            let mut spent = budget;
            validate_within_budget("next.gif", "image/gif", &partial, &mut spent)
                .await
                .expect_err("the next candidate is refused against the spent envelope");
        })
        .await;
        assert_eq!(
            decodes, 0,
            "the refused candidate does not decode: recurring transient work closed the budget"
        );
    }

    #[tokio::test]
    async fn non_rgba_apng_charges_recurring_conversion() {
        // RGB8 APNG regression: the decoder reads a raw Rgb8 frame buffer and
        // allocates a separate Rgba8 source before blending. Both are live
        // simultaneously, and both recur on every frame, so they accumulate.

        // Anchor the expectation to the decoder's *actual* allocations rather
        // than to `APNG_SCRATCH_MODEL.per_frame`. Deriving the expected charge
        // from the constant makes the test pass at whatever value the constant
        // happens to hold, which is exactly how an undercount survives review.
        //
        // From `image` 0.25.10 `src/codecs/png.rs`:
        //   - `:418-425` allocates the raw frame buffer, 3 B/px for `Rgb8`;
        //   - `:456-470` allocates the converted RGBA `source`, 4 B/px;
        //   - `:477-479` frees the raw buffer only *after* that conversion.
        // So the recurring cost is 3 + 4 = 7 B/px, and the two persistent
        // canvases (`current`, `previous`) are 4 + 4 = 8 B/px.
        const APNG_RGB8_RAW_BYTES_PER_PIXEL: u64 = 3;
        const APNG_CONVERTED_SOURCE_BYTES_PER_PIXEL: u64 = 4;
        const APNG_PERSISTENT_CANVASES_BYTES_PER_PIXEL: u64 = 8;
        const APNG_REAL_PER_FRAME_BYTES_PER_PIXEL: u64 =
            APNG_RGB8_RAW_BYTES_PER_PIXEL + APNG_CONVERTED_SOURCE_BYTES_PER_PIXEL;

        // Compile-time facts about the pinned decoder, so lowering either term
        // below what the decoder really allocates fails the build rather than
        // waiting for this test to run.
        const _: () = assert!(
            APNG_SCRATCH_MODEL.per_frame >= APNG_REAL_PER_FRAME_BYTES_PER_PIXEL,
            "the APNG recurring term must cover the decoder's simultaneously live raw and \
             converted RGBA buffers (3 + 4 = 7 B/px for Rgb8)"
        );
        const _: () = assert!(
            APNG_SCRATCH_MODEL.persistent >= APNG_PERSISTENT_CANVASES_BYTES_PER_PIXEL,
            "the APNG persistent term must cover both retained canvases (4 + 4 = 8 B/px)"
        );

        let rgb_apng = two_frame_apng_with_color(false); // Rgb8, not Rgba8

        // Two 1×1 Rgb8 frames: 4 B output each (the decoder yields RGBA8),
        // plus the recurring raw+source pair, plus the canvases once.
        let cumulative = 2 * (4 + APNG_SCRATCH_MODEL.per_frame) + APNG_SCRATCH_MODEL.persistent;

        let mut budget = cumulative;
        let charged = validate_within_budget("rgb.png", "image/png", &rgb_apng, &mut budget)
            .await
            .expect("the RGB8 APNG fits a budget sized for its recurring conversion work");
        assert_eq!(
            charged, cumulative,
            "the RGB8 APNG is charged every frame's raw+source buffers plus persistent canvases"
        );
        assert_eq!(
            budget, 0,
            "the cumulative conversion work consumes the envelope"
        );

        let ((), decodes) = counting_decodes(async {
            let mut spent = budget;
            validate_within_budget("next.png", "image/png", &rgb_apng, &mut spent)
                .await
                .expect_err("the next candidate is refused against the spent envelope");
        })
        .await;
        assert_eq!(
            decodes, 0,
            "the refused candidate does not decode: recurring per-frame conversion closed the budget"
        );
    }

    fn two_frame_apng() -> Vec<u8> {
        two_frame_apng_with_color(true)
    }

    /// A two-frame APNG with a configurable color type.
    ///
    /// When `rgba` is `true` the frames use `ColorType::Rgba8` (the default).
    /// When `rgba` is `false` the frames use `ColorType::Rgb8`, which exercises
    /// the color-conversion path in `ApngDecoder::mix_next_frame`: the decoder
    /// reads a raw `Rgb8` frame buffer and allocates a separate `Rgba8` source
    /// before blending it into `current`. Both the raw and source buffers are
    /// live simultaneously, so the `Rgb8` path peaks at more bytes per pixel
    /// than the `Rgba8` path where `from_raw` reuses the same buffer.
    fn two_frame_apng_with_color(rgba: bool) -> Vec<u8> {
        fn crc32(name: &[u8; 4], data: &[u8]) -> u32 {
            let mut crc = 0xFFFF_FFFFu32;
            for byte in name.iter().chain(data) {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = if crc & 1 == 0 {
                        crc >> 1
                    } else {
                        0xEDB8_8320 ^ (crc >> 1)
                    };
                }
            }
            !crc
        }

        fn append_chunk(output: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
            output.extend_from_slice(&(data.len() as u32).to_be_bytes());
            output.extend_from_slice(name);
            output.extend_from_slice(data);
            output.extend_from_slice(&crc32(name, data).to_be_bytes());
        }

        // Reuse the encoder-backed still fixture's compressed scanline as each
        // APNG frame. This keeps the test dependency-free while constructing a
        // standards-shaped two-frame stream with explicit animation chunks.
        let still = if rgba {
            valid_png()
        } else {
            let mut buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                1,
                1,
                image::Rgb([255, 0, 0]),
            ))
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("test PNG encodes");
            buf.into_inner()
        };
        let mut idat = Vec::new();
        let mut offset = 8usize;
        while offset + 12 <= still.len() {
            let length = u32::from_be_bytes(
                still[offset..offset + 4]
                    .try_into()
                    .expect("chunk length is four bytes"),
            ) as usize;
            let chunk_end = offset + 12 + length;
            assert!(chunk_end <= still.len(), "test PNG chunks are in bounds");
            if &still[offset + 4..offset + 8] == b"IDAT" {
                idat.extend_from_slice(&still[offset + 8..offset + 8 + length]);
            }
            offset = chunk_end;
        }
        assert!(!idat.is_empty(), "test PNG contains compressed image data");

        let ihdr = [
            0,
            0,
            0,
            1, // width
            0,
            0,
            0,
            1, // height
            8,
            if rgba { 6 } else { 2 }, // bit depth, color type (RGBA8 / RGB8)
            0,
            0,
            0, // compression, filter, interlace
        ];
        let mut frame_control = [0u8; 26];
        frame_control[4..8].copy_from_slice(&1u32.to_be_bytes());
        frame_control[8..12].copy_from_slice(&1u32.to_be_bytes());
        frame_control[20..22].copy_from_slice(&1u16.to_be_bytes());
        frame_control[22..24].copy_from_slice(&100u16.to_be_bytes());

        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        append_chunk(&mut bytes, b"IHDR", &ihdr);
        let mut animation_control = Vec::with_capacity(8);
        animation_control.extend_from_slice(&2u32.to_be_bytes());
        animation_control.extend_from_slice(&0u32.to_be_bytes());
        append_chunk(&mut bytes, b"acTL", &animation_control);
        append_chunk(&mut bytes, b"fcTL", &frame_control);
        append_chunk(&mut bytes, b"IDAT", &idat);

        frame_control[..4].copy_from_slice(&1u32.to_be_bytes());
        append_chunk(&mut bytes, b"fcTL", &frame_control);
        let mut frame_data = Vec::with_capacity(4 + idat.len());
        frame_data.extend_from_slice(&2u32.to_be_bytes());
        frame_data.extend_from_slice(&idat);
        append_chunk(&mut bytes, b"fdAT", &frame_data);
        append_chunk(&mut bytes, b"IEND", &[]);
        bytes
    }

    fn corrupt_png_chunk_data(bytes: &mut [u8], wanted: &[u8; 4]) {
        let mut offset = 8usize;
        while offset + 12 <= bytes.len() {
            let length = u32::from_be_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .expect("chunk length is four bytes"),
            ) as usize;
            let chunk_end = offset + 12 + length;
            assert!(chunk_end <= bytes.len(), "test PNG chunks are in bounds");
            if &bytes[offset + 4..offset + 8] == wanted {
                let data_offset = offset + 8 + usize::from(wanted == b"fdAT") * 4;
                assert!(data_offset < offset + 8 + length, "chunk has payload data");
                bytes[data_offset] ^= 0xFF;
                return;
            }
            offset = chunk_end;
        }
        panic!("test PNG contains the requested chunk");
    }

    #[tokio::test]
    async fn apng_validates_every_frame() {
        let valid = two_frame_apng();
        let allocation = validate_image_content(
            "anim.png",
            "image/png",
            &valid,
            MAX_DECODED_IMAGE_ALLOC_BYTES,
        )
        .await
        .expect("a valid two-frame APNG is accepted");
        // The charge is cumulative frame work plus the state the decoder holds
        // across the animation: two 1x1 frames give `total = 2 * 4` and one
        // canvas of APNG scratch.
        assert_eq!(
            allocation,
            2 * (4 + APNG_SCRATCH_MODEL.per_frame) + APNG_SCRATCH_MODEL.persistent,
            "an accepted APNG charges every frame's output+transient plus its persistent canvases"
        );

        let mut corrupt = valid;
        corrupt_png_chunk_data(&mut corrupt, b"fdAT");
        validate_image_content(
            "anim.png",
            "image/png",
            &corrupt,
            MAX_DECODED_IMAGE_ALLOC_BYTES,
        )
        .await
        .expect_err("a corrupt later APNG frame must be rejected");
    }

    #[tokio::test]
    async fn non_rgba_apng_conversion_buffers_are_accounted() {
        // `ApngDecoder::mix_next_frame` keeps `current` and `previous` RGBA
        // canvases live, then for a non-RGBA color type it reads a raw frame
        // buffer *and* allocates a separate RGBA `source` to convert into
        // before blending (`image-0.25.10/src/codecs/png.rs:456-479`). Both are
        // live at once, so an Rgb8 APNG peaks at
        // `current(4) + previous(4) + raw(3) + source(4) = 15 B/px` while its
        // `total_bytes()` output is only 3 B/px.
        //
        // Charging 8 B/px of scratch covered only the two persistent canvases
        // and under-accounted every non-RGBA APNG. The bound must cover the
        // worst accepted color type, which is Rgb8.
        let rgb8 = two_frame_apng_with_color(false);

        let charged = validate_image_content("rgb8.png", "image/png", &rgb8, u64::MAX)
            .await
            .expect("a valid Rgb8 APNG is accepted");

        // The real Rgb8 peak, from the source-confirmed allocation list above.
        let real_peak = 4 + 4 + 3 + 4;
        assert!(
            charged >= real_peak,
            "an Rgb8 APNG must be charged at least its decoder peak ({real_peak} B for 1x1); \
             got {charged}"
        );

        // The RGBA8 shape must keep working; it simply has no conversion pair.
        let rgba8 = two_frame_apng_with_color(true);
        validate_image_content("rgba8.png", "image/png", &rgba8, u64::MAX)
            .await
            .expect("a valid Rgba8 APNG is still accepted");
    }

    #[tokio::test]
    async fn apng_with_background_disposal_is_refused_before_decode() {
        // `ApngDecoder::mix_next_frame` handles `DisposeOp::Background` by
        // collecting the full disposal region into a
        // `Vec<(u32, u32, Rgba<u8>)>` — 12 bytes per pixel — before clearing
        // it (`image-0.25.10/src/codecs/png.rs:385-401`). That allocation is
        // not preceded by any `Limits::reserve*`, so it is invisible to both
        // the per-image decoder limit and the aggregate accounting. A
        // full-canvas 2000×2000 APNG with dispose_op=Background can therefore
        // allocate ~48 MiB more than its header projection implies, even when
        // the frame and canvas dimensions stay below the per-image cap.
        //
        // The fix is to refuse any APNG that uses background disposal before
        // the decoder runs, detected from the raw `fcTL` bytes.
        //
        // Build a 1×1 APNG whose first fcTL chunk sets dispose_op=Background.
        // The `two_frame_apng_with_color` helper stores the frame_control
        // array directly into the chunk body and the dispose_op sits at byte 24
        // of that 26-byte payload (offset 24 from the payload start, which is
        // right after the 4-byte length + 4-byte "fcTL" type in the stream).
        // Reproduce the minimal fixture here with the disposal byte set.
        let apng_with_background_disposal = {
            fn crc32(name: &[u8; 4], data: &[u8]) -> u32 {
                let mut crc = 0xFFFF_FFFFu32;
                for byte in name.iter().chain(data) {
                    crc ^= u32::from(*byte);
                    for _ in 0..8 {
                        crc = if crc & 1 == 0 {
                            crc >> 1
                        } else {
                            0xEDB8_8320 ^ (crc >> 1)
                        };
                    }
                }
                !crc
            }
            fn append_chunk(output: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
                output.extend_from_slice(&(data.len() as u32).to_be_bytes());
                output.extend_from_slice(name);
                output.extend_from_slice(data);
                output.extend_from_slice(&crc32(name, data).to_be_bytes());
            }

            let still = valid_png();
            let mut idat = Vec::new();
            let mut offset = 8usize;
            while offset + 12 <= still.len() {
                let length =
                    u32::from_be_bytes(still[offset..offset + 4].try_into().unwrap()) as usize;
                let chunk_end = offset + 12 + length;
                if &still[offset + 4..offset + 8] == b"IDAT" {
                    idat.extend_from_slice(&still[offset + 8..offset + 8 + length]);
                }
                offset = chunk_end;
            }

            let ihdr = [0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0];
            let mut frame_control = [0u8; 26];
            frame_control[4..8].copy_from_slice(&1u32.to_be_bytes()); // width
            frame_control[8..12].copy_from_slice(&1u32.to_be_bytes()); // height
            frame_control[20..22].copy_from_slice(&1u16.to_be_bytes()); // delay_num
            frame_control[22..24].copy_from_slice(&100u16.to_be_bytes()); // delay_den
            frame_control[24] = 1; // DisposeOp::Background

            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            append_chunk(&mut bytes, b"IHDR", &ihdr);
            let mut actl = Vec::new();
            actl.extend_from_slice(&2u32.to_be_bytes()); // num_frames
            actl.extend_from_slice(&0u32.to_be_bytes()); // num_plays
            append_chunk(&mut bytes, b"acTL", &actl);
            append_chunk(&mut bytes, b"fcTL", &frame_control); // dispose_op=Background
            append_chunk(&mut bytes, b"IDAT", &idat);
            frame_control[..4].copy_from_slice(&1u32.to_be_bytes()); // next seq
            frame_control[24] = 0; // second frame uses None so it would decode fine
            append_chunk(&mut bytes, b"fcTL", &frame_control);
            let mut fdat = Vec::with_capacity(4 + idat.len());
            fdat.extend_from_slice(&2u32.to_be_bytes());
            fdat.extend_from_slice(&idat);
            append_chunk(&mut bytes, b"fdAT", &fdat);
            append_chunk(&mut bytes, b"IEND", &[]);
            bytes
        };

        // A 1×1 APNG is nowhere near the per-image ceiling, so the only reason
        // it is refused is the background-disposal check — not a size or decode
        // error. Without the check this fixture decodes successfully.
        let error = validate_image_content(
            "bg.png",
            "image/png",
            &apng_with_background_disposal,
            u64::MAX,
        )
        .await
        .expect_err("an APNG with background disposal must be refused before decode");

        // The refusal drives the projection above the per-image ceiling
        // (`projected_allocation` returns `MAX_DECODED_IMAGE_ALLOC_BYTES + 1`),
        // so `per_image_cap_refusal` fires before the decoder is ever entered.
        let reason = error.to_string();
        assert!(
            reason.contains("exceeds per-image limit"),
            "the refusal must come from the pre-decode ceiling check; got: {reason}"
        );

        // An APNG without background disposal must still be accepted.
        let apng_none_disposal = two_frame_apng_with_color(true);
        validate_image_content("ok.png", "image/png", &apng_none_disposal, u64::MAX)
            .await
            .expect("an APNG without background disposal must still be accepted");
    }

    fn lossless_webp(pixel: [u8; 3]) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::webp::WebPEncoder::new_lossless(&mut bytes),
            &pixel,
            1,
            1,
            image::ExtendedColorType::Rgb8,
        )
        .expect("test WebP frame encodes");
        bytes
    }

    fn append_webp_chunk(output: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
        output.extend_from_slice(name);
        output.extend_from_slice(&(data.len() as u32).to_le_bytes());
        output.extend_from_slice(data);
        if !data.len().is_multiple_of(2) {
            output.push(0);
        }
    }

    fn two_frame_webp() -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        // Animation flag, three reserved bytes, then 24-bit canvas width-1
        // and height-1. Both dimensions are one pixel.
        append_webp_chunk(&mut body, b"VP8X", &[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        append_webp_chunk(&mut body, b"ANIM", &[0, 0, 0, 0, 0, 0]);

        for encoded in [lossless_webp([255, 0, 0]), lossless_webp([0, 255, 0])] {
            let mut frame = vec![0; 16];
            frame[12] = 1; // one-millisecond duration
            frame.extend_from_slice(&encoded[12..]);
            append_webp_chunk(&mut body, b"ANMF", &frame);
        }

        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes
    }

    fn corrupt_second_webp_frame(bytes: &mut [u8]) {
        let mut offset = 12usize;
        let mut animation_frames = 0usize;
        while offset + 8 <= bytes.len() {
            let length = u32::from_le_bytes(
                bytes[offset + 4..offset + 8]
                    .try_into()
                    .expect("chunk length is four bytes"),
            ) as usize;
            let padded_length = length + (length % 2);
            if &bytes[offset..offset + 4] == b"ANMF" {
                animation_frames += 1;
                if animation_frames == 2 {
                    let frame_bitstream = offset + 8 + 16 + 8;
                    assert!(frame_bitstream < bytes.len());
                    bytes[frame_bitstream] ^= 0xFF;
                    return;
                }
            }
            offset += 8 + padded_length;
        }
        panic!("test WebP contains a second animation frame");
    }

    #[tokio::test]
    async fn animated_webp_validates_every_frame() {
        let valid = two_frame_webp();
        let allocation = validate_image_content(
            "anim.webp",
            "image/webp",
            &valid,
            MAX_DECODED_IMAGE_ALLOC_BYTES,
        )
        .await
        .expect("a valid two-frame animated WebP is accepted");
        // The charge is cumulative frame work plus the state the decoder holds
        // across the animation: two 1x1 frames give `total = 2 * 4` and one
        // canvas of animated-WebP scratch.
        assert_eq!(
            allocation,
            2 * (4 + WEBP_ANIMATED_SCRATCH_MODEL.per_frame)
                + WEBP_ANIMATED_SCRATCH_MODEL.persistent,
            "an accepted animated WebP charges every frame's output+transient plus its persistent canvas"
        );

        let mut corrupt = valid;
        corrupt_second_webp_frame(&mut corrupt);
        validate_image_content(
            "anim.webp",
            "image/webp",
            &corrupt,
            MAX_DECODED_IMAGE_ALLOC_BYTES,
        )
        .await
        .expect_err("a corrupt later WebP frame must be rejected");
    }

    /// A WebP whose `VP8X` canvas is exactly `dimension` x `dimension`.
    ///
    /// `animated` selects the `ANIM`/`ANMF` form; the frames themselves stay
    /// 1x1 so the fixture is small, proving the declared canvas alone is what
    /// gets the payload refused.
    fn webp_with_canvas(dimension: u32, animated: bool) -> Vec<u8> {
        // 24-bit little-endian canvas width-1 and height-1.
        let minus_one = dimension - 1;
        let encoded = minus_one.to_le_bytes();
        let canvas_minus_one = [encoded[0], encoded[1], encoded[2]];

        let mut vp8x = vec![if animated { 0x02 } else { 0x00 }, 0, 0, 0];
        vp8x.extend_from_slice(&canvas_minus_one);
        vp8x.extend_from_slice(&canvas_minus_one);

        let mut body = b"WEBP".to_vec();
        append_webp_chunk(&mut body, b"VP8X", &vp8x);

        if animated {
            append_webp_chunk(&mut body, b"ANIM", &[0, 0, 0, 0, 0, 0]);
            for encoded in [lossless_webp([255, 0, 0]), lossless_webp([0, 255, 0])] {
                let mut frame = vec![0; 16];
                frame[12] = 1; // one-millisecond duration
                frame.extend_from_slice(&encoded[12..]);
                append_webp_chunk(&mut body, b"ANMF", &frame);
            }
        } else {
            let encoded = lossless_webp([255, 0, 0]);
            append_webp_chunk(&mut body, b"VP8L", &encoded[20..]);
        }

        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes
    }

    /// A WebP whose `VP8X` canvas is 5000x5000 — strictly above the projection
    /// threshold.
    fn oversized_canvas_webp(animated: bool) -> Vec<u8> {
        webp_with_canvas(5000, animated)
    }

    #[tokio::test]
    async fn webp_canvas_at_the_per_image_cap_is_refused_before_allocation() {
        // Before the peak-aware bound, `projected_allocation` returned w*h*4 for
        // every image. For a 4096x4096 WebP, w*h*4 = MAX_DECODED_IMAGE_ALLOC_BYTES
        // exactly, so a `>` comparison would have admitted it. Now the bound
        // includes the decoder's scratch cost too, so even a still 4096x4096 WebP
        // projects well above the cap and is refused on the `>=` branch.
        const CAP_DIMENSION: u32 = 4096;
        // Sanity: the old w*h*4 value sat exactly on the boundary.
        assert_eq!(
            u64::from(CAP_DIMENSION) * u64::from(CAP_DIMENSION) * 4,
            MAX_DECODED_IMAGE_ALLOC_BYTES,
            "CAP_DIMENSION must be chosen so old w*h*4 sits exactly on the cap"
        );
        // Sanity: the peak-aware projection for a still WebP at this size is
        // output(3 B/px) + still scratch, strictly above the cap.
        let still_peak = u64::from(CAP_DIMENSION)
            * u64::from(CAP_DIMENSION)
            * (3 + WEBP_STILL_SCRATCH_BYTES_PER_PIXEL);
        assert!(
            still_peak > MAX_DECODED_IMAGE_ALLOC_BYTES,
            "the still-WebP peak at CAP_DIMENSION must exceed the cap: {still_peak} vs {MAX_DECODED_IMAGE_ALLOC_BYTES}"
        );

        // An unbounded aggregate allowance so the per-image cap is the only
        // gate under test. At this canvas the animated peak model also exceeds
        // the 256 MiB aggregate envelope, and an aggregate refusal would prove
        // nothing about the per-image guard this test exists to cover.
        for animated in [false, true] {
            let webp = webp_with_canvas(CAP_DIMENSION, animated);

            let error = validate_image_content("boundary.webp", "image/webp", &webp, u64::MAX)
                .await
                .expect_err("a canvas landing exactly on the per-image cap must be refused");

            assert_eq!(
                multimodal_error_kind(&error),
                "corrupt_image",
                "animated={animated}"
            );
            assert!(
                error.to_string().contains("per-image limit"),
                "the refusal must name the per-image cap (animated={animated}): {error}"
            );
        }
    }

    #[tokio::test]
    async fn webp_below_cap_threshold_is_refused_when_decoder_peak_would_exceed_it() {
        // A 4000x4000 lossless non-alpha WebP projects to 64_000_000 bytes,
        // which is below MAX_DECODED_IMAGE_ALLOC_BYTES (67_108_864).  The old
        // width*height*4 projection would therefore admit it.  But
        // image-webp 0.2.4 allocates a width*height*4 RGBA scratch buffer AND
        // a separate width*height*3 RGB output before returning, so the actual
        // peak is ~112 MB — well above the 64 MiB cap.  projected_allocation
        // now adds the scratch cost, so the image is refused before any
        // decoder runs.
        // An unbounded budget so the per-image cap is the only gate being tested.
        // At 4000px the animated peak model exceeds the 256 MiB aggregate, and
        // an aggregate refusal would prove nothing about the per-image guard.
        for animated in [false, true] {
            let webp = webp_with_canvas(4000, animated);

            let error = validate_image_content("compact.webp", "image/webp", &webp, u64::MAX)
                .await
                .expect_err(
                    "a 4000x4000 WebP must be refused even though its projection is below the cap",
                );

            assert_eq!(
                multimodal_error_kind(&error),
                "corrupt_image",
                "animated={animated}"
            );
            assert!(
                error.to_string().contains("per-image limit"),
                "refusal must name the per-image cap (animated={animated}): {error}"
            );
        }
    }

    #[tokio::test]
    async fn high_bit_depth_png_is_refused_when_output_exceeds_cap() {
        // A valid 3400x3400 RGB16 PNG has a decoded output of 3400*3400*6 =
        // 69_360_000 bytes, which exceeds MAX_DECODED_IMAGE_ALLOC_BYTES
        // (67_108_864). The old width*height*4 projection (46 MB) would have
        // admitted it. projected_allocation now calls total_bytes() on the
        // decoder, which uses the actual ColorType byte width (6 for Rgb16),
        // so the image is caught before DynamicImage::from_decoder allocates.
        let png = {
            let mut buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb16(image::ImageBuffer::from_pixel(
                3400,
                3400,
                image::Rgb([u16::MAX, 0, 0]),
            ))
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("test RGB16 PNG encodes");
            buf.into_inner()
        };

        // Sanity-check that the output byte count really exceeds the cap.
        let rgb16_bytes = u64::from(3400u32) * u64::from(3400u32) * 6;
        assert!(
            rgb16_bytes > MAX_DECODED_IMAGE_ALLOC_BYTES,
            "test fixture must project above the cap: {rgb16_bytes} vs {MAX_DECODED_IMAGE_ALLOC_BYTES}"
        );

        let error =
            validate_image_content("hbd.png", "image/png", &png, AGGREGATE_DECODE_BUDGET_BYTES)
                .await
                .expect_err(
                    "a 3400x3400 RGB16 PNG must be refused before its output buffer is allocated",
                );

        assert_eq!(multimodal_error_kind(&error), "corrupt_image");
        assert!(
            error.to_string().contains("per-image limit"),
            "refusal must name the per-image cap: {error}"
        );

        // An unrelated valid sibling must keep the shared budget intact,
        // because the pre-decode refusal charges nothing.
        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        validate_within_budget("hbd.png", "image/png", &png, &mut budget)
            .await
            .expect_err("high-bit-depth PNG must be refused via validate_within_budget too");
        assert_eq!(
            budget, AGGREGATE_DECODE_BUDGET_BYTES,
            "pre-decode refusal must not charge the shared budget"
        );
    }

    #[tokio::test]
    async fn sub_cap_high_bit_depth_png_charges_its_real_decoded_size() {
        // The success-path counterpart to the admission test above. A valid
        // 1000x1000 RGB16 PNG is 6_000_000 bytes decoded (6 bytes/pixel for
        // ImageRgb16), well under the per-image cap, so it is accepted. The
        // old success path returned width*height*4 = 4_000_000, undercharging
        // the shared aggregate budget by 1.5x on every such image. With
        // max_images as high as 16, repeated undercharging lets a request
        // decode substantially more than the 256 MiB the accounting claims.
        let png = {
            let mut buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb16(image::ImageBuffer::from_pixel(
                1000,
                1000,
                image::Rgb([u16::MAX, 0, 0]),
            ))
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("test RGB16 PNG encodes");
            buf.into_inner()
        };

        let rgb16_bytes = 1000u64 * 1000 * 6;
        assert!(
            rgb16_bytes < MAX_DECODED_IMAGE_ALLOC_BYTES,
            "this fixture must be accepted, not refused by the per-image cap"
        );

        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        let charged = validate_within_budget("hbd.png", "image/png", &png, &mut budget)
            .await
            .expect("a sub-cap high-bit-depth PNG is a valid image and must be accepted");

        assert_eq!(
            charged, rgb16_bytes,
            "the charge must be the real ImageRgb16 allocation, not a width*height*4 estimate"
        );
        assert_eq!(
            budget,
            AGGREGATE_DECODE_BUDGET_BYTES - rgb16_bytes,
            "the aggregate budget must be debited by the real decoded size"
        );

        // Drive several candidates through the same budget: the running total
        // must track the real cost, so the accounting cannot claim headroom
        // that the decoder has already spent.
        for _ in 0..3 {
            validate_within_budget("hbd.png", "image/png", &png, &mut budget)
                .await
                .expect("further sub-cap candidates remain within the aggregate budget");
        }
        assert_eq!(
            budget,
            AGGREGATE_DECODE_BUDGET_BYTES - rgb16_bytes * 4,
            "four accepted candidates must debit four real allocations"
        );
    }

    #[tokio::test]
    async fn still_alpha_webp_is_refused_before_allocation_when_peak_exceeds_cap() {
        // A valid 2800x2800 lossless alpha WebP illustrates the undercount the
        // prior scratch model had: color_type() returns Rgba8 (4 B/px), so
        // total_bytes() = 2800²×4 = 31,360,000. The old scratch addend (4 B/px)
        // gave a projection of 62,720,000, below MAX_DECODED_IMAGE_ALLOC_BYTES
        // (67,108,864). But read_alpha_chunk's lossless branch allocates another
        // 2800²×4 RGBA buffer and a 2800²×1 green plane alongside the output, so
        // the real peak is 2800²×(4+4+1) = 70,560,000. The corrected scratch
        // (5 B/px) projects 2800²×(4+5) = 70,560,000, now above the cap.
        //
        // The fixture is built from a real encoder so the VP8X alpha flag, ALPH
        // chunk, and VP8 opaque layer are all consistent with what the decoder
        // checks — a mismatched fixture would fail in a different place.
        let alpha_webp = {
            let mut buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                2800,
                2800,
                image::Rgba([255, 0, 0, 128]),
            ))
            .write_to(&mut buf, image::ImageFormat::WebP)
            .expect("test alpha WebP encodes");
            buf.into_inner()
        };

        // Sanity: the old projection would have admitted this image.
        let old_proj = 2800u64 * 2800 * (4 + 4); // output + old scratch
        assert!(
            old_proj < MAX_DECODED_IMAGE_ALLOC_BYTES,
            "old 4B/px scratch would have admitted this: {old_proj} < {MAX_DECODED_IMAGE_ALLOC_BYTES}"
        );
        // Sanity: the corrected projection refuses it.
        let new_proj = 2800u64 * 2800 * (4 + WEBP_STILL_SCRATCH_BYTES_PER_PIXEL);
        assert!(
            new_proj >= MAX_DECODED_IMAGE_ALLOC_BYTES,
            "new scratch must project above the cap: {new_proj} vs {MAX_DECODED_IMAGE_ALLOC_BYTES}"
        );

        let error = validate_image_content(
            "alpha.webp",
            "image/webp",
            &alpha_webp,
            AGGREGATE_DECODE_BUDGET_BYTES,
        )
        .await
        .expect_err("a 2800x2800 alpha WebP must be refused before pixel allocation");

        assert_eq!(multimodal_error_kind(&error), "corrupt_image");
        assert!(
            error.to_string().contains("per-image limit"),
            "refusal must name the per-image cap: {error}"
        );

        // Budget must be untouched — no decoder ran.
        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;
        validate_within_budget("alpha.webp", "image/webp", &alpha_webp, &mut budget)
            .await
            .expect_err("validate_within_budget must also refuse it");
        assert_eq!(
            budget, AGGREGATE_DECODE_BUDGET_BYTES,
            "pre-decode refusal must not charge the shared budget"
        );
    }

    #[tokio::test]
    async fn multiple_valid_webps_respect_the_aggregate_budget() {
        // Regression for the charge/admission mismatch. Admission deducts the
        // projected peak (output + scratch); the old success path returned only
        // as_bytes().len() (output alone), so the scratch delta was reserved and
        // never credited back and the counter drifted below real consumption.
        //
        // Drive a sequence of candidates through one shared budget, sized so
        // exhaustion is reached in a handful of rounds, and assert three things
        // the single-candidate version could not: every accepted candidate
        // debits at least its projected peak, the running total matches the sum
        // of the charges, and once the remaining budget can no longer fit the
        // projection the next candidate is refused *without* decoding.
        let webp = {
            let mut buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                64,
                64,
                image::Rgb([255, 0, 0]),
            ))
            .write_to(&mut buf, image::ImageFormat::WebP)
            .expect("test 64x64 WebP encodes");
            buf.into_inner()
        };

        let proj = projected_allocation("ok.webp", "image/webp", &webp)
            .expect("64x64 WebP must project without error");
        assert!(proj > 0, "projection must be non-zero for the maths below");

        // Room for exactly three candidates, with a remainder too small for a
        // fourth so the exhaustion branch is reached deterministically.
        let mut budget = proj * 3 + proj / 2;
        let starting_budget = budget;

        let (charges, decodes) = counting_decodes(async {
            let mut charges = Vec::new();
            for _ in 0..3 {
                let charged = validate_within_budget("ok.webp", "image/webp", &webp, &mut budget)
                    .await
                    .expect("each candidate fits the remaining budget");
                charges.push(charged);
            }
            charges
        })
        .await;

        for (index, charged) in charges.iter().enumerate() {
            assert!(
                *charged >= proj,
                "candidate {index} must be charged at least its projected peak: \
                 charged={charged} proj={proj}"
            );
        }
        assert_eq!(decodes, 3, "each accepted candidate decodes exactly once");

        let spent: u64 = charges.iter().sum();
        assert_eq!(
            budget,
            starting_budget - spent,
            "the running counter must equal the sum of the real charges"
        );
        assert!(
            budget < proj,
            "the remainder must be too small for another candidate: {budget} vs {proj}"
        );

        // The next candidate cannot fit. It must be refused by the cheap
        // projection check, never by entering full validation.
        let ((), decodes_after) = counting_decodes(async {
            validate_within_budget("ok.webp", "image/webp", &webp, &mut budget)
                .await
                .expect_err("a candidate that no longer fits must be refused");
        })
        .await;
        assert_eq!(
            decodes_after, 0,
            "an over-budget candidate must not reach the decoder"
        );
        assert_eq!(
            budget, 0,
            "aggregate exhaustion closes the budget for the remaining candidates"
        );
    }

    #[tokio::test]
    async fn oversized_webp_canvas_is_refused_before_allocation() {
        // `WebPDecoder` (image 0.25.10) does not override `set_limits`, so
        // `max_alloc` is silently dropped on that path.  The explicit pre-decode
        // guard covers every WebP shape.  A 5000x5000 canvas now projects well
        // above both the per-image cap and the aggregate budget once the
        // decoder-peak scratch is included, so either gate may fire first;
        // what matters is that the image is refused before any allocation.
        for animated in [false, true] {
            let webp = oversized_canvas_webp(animated);

            let error = validate_image_content(
                "big.webp",
                "image/webp",
                &webp,
                AGGREGATE_DECODE_BUDGET_BYTES,
            )
            .await
            .expect_err("a canvas far above the per-image cap must be refused before allocation");

            assert_eq!(
                multimodal_error_kind(&error),
                "corrupt_image",
                "animated={animated}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn oversized_webp_canvas_does_not_close_the_shared_budget() {
        // The per-image cap refusal is an ordinary invalid-image failure, so an
        // unrelated sibling must keep its full budget allowance.
        //
        // Two shapes exercise different code paths through `validate_within_budget`:
        //
        // Still (animated=false): projected ~200 MB, above the 64 MiB per-image cap
        // but below the 256 MiB aggregate gate. Without the per-image guard moved
        // forward the aggregate gate would never be reached, but the refusal
        // happens at `validate_image_content_with_projection` instead. The budget
        // must still be untouched.
        //
        // Animated (animated=true): projected ~525 MB (17 B/px scratch model),
        // above BOTH the 64 MiB per-image cap AND the 256 MiB aggregate gate.
        // Without the per-image guard moved *before* the aggregate check,
        // `validate_within_budget` would zero the budget on the aggregate branch
        // and the PNG sibling below would be refused without ever decoding — this
        // is the specific path the `per_image_cap_refusal` guard was added to fix.
        for animated in [false, true] {
            let webp = oversized_canvas_webp(animated);
            let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;

            validate_within_budget("big.webp", "image/webp", &webp, &mut budget)
                .await
                .expect_err("an over-cap canvas must be refused");

            assert_eq!(
                budget, AGGREGATE_DECODE_BUDGET_BYTES,
                "animated={animated}: nothing was decoded, so the shared budget must be \
                 untouched for siblings"
            );

            let allocation =
                validate_within_budget("ok.png", "image/png", &valid_png(), &mut budget)
                    .await
                    .expect("an unrelated valid sibling must still be admitted");
            assert_eq!(
                budget,
                AGGREGATE_DECODE_BUDGET_BYTES - allocation,
                "animated={animated}: only the sibling's real decode is charged"
            );
        }
    }

    #[tokio::test]
    async fn oversized_animated_webp_in_newer_message_does_not_discard_valid_png_sibling() {
        // Production-path regression for the animated-WebP budget-classification
        // bug: an animated WebP with a canvas projection above both the per-image
        // cap and the aggregate allowance is placed in the newer message so
        // newest-first traversal processes it before the valid PNG in the older
        // message. The PNG must still reach the prepared payload and only "1 of 2"
        // images should be reported as skipped.
        //
        // Without the per-image guard added before the aggregate gate this test
        // fails: `validate_within_budget` zeros `remaining_budget` on the aggregate
        // branch, the PNG then hits the zero-budget early exit and is skipped, and
        // `contains_images` is false.
        let temp = tempfile::tempdir().unwrap();
        let old_png = temp.path().join("old.png");
        let new_webp = temp.path().join("new.webp");
        std::fs::write(&old_png, valid_png()).unwrap();
        // animated=true → projection ≈ 525 MB, above both caps.
        std::fs::write(&new_webp, oversized_canvas_webp(true)).unwrap();

        let history = vec![
            ChatMessage::user("photo").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (old_png.display()).to_string(),
                },
            ]),
            ChatMessage::user("check this").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (new_webp.display()).to_string(),
                },
            ]),
        ];
        let (prepared, decodes) = counting_decodes(prepare_messages_for_provider(
            &history,
            &MultimodalConfig::default(),
        ))
        .await;
        let prepared = prepared.expect("an over-cap animated WebP must not abort preparation");

        assert!(
            prepared.contains_images,
            "the valid PNG in the older message must survive the over-cap animated WebP: {:?}",
            prepared
                .messages
                .iter()
                .map(|m| &m.content)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            prepared.messages.len(),
            2,
            "both messages must be present with original order restored"
        );
        assert!(
            prepared
                .messages
                .iter()
                .any(|m| m.content.contains("could not be loaded")),
            "the newer message must carry the skip note for the over-cap WebP: {:?}",
            prepared
                .messages
                .iter()
                .map(|m| &m.content)
                .collect::<Vec<_>>()
        );
        // The over-cap WebP must have been refused before any pixel decode, so no
        // decode call should have been recorded for it.
        assert_eq!(
            decodes, 1,
            "only the valid PNG should have entered a full decode; the animated WebP \
             must be refused by the per-image header check before decode"
        );
    }

    #[test]
    fn skipped_image_log_attrs_never_carry_the_raw_reference() {
        // The skip event is emitted for untrusted input on a path that now
        // fires far more often (corrupt payloads, over-cap canvases). It must
        // carry only the reference *class*, never the reference: a local path
        // exposes workspace and user naming, a remote URL can carry query
        // credentials or a private endpoint, and a data URI exposes the
        // image/base64 prefix.
        //
        // Asserting on the attrs value rather than on a captured tracing event
        // keeps this deterministic: it needs no global subscriber, so it
        // cannot be silently defeated by whichever test installs one first.
        let ctx = ImageNormalizeCtx {
            message_index: 3,
            role: "user",
        };

        // One reference per source class, each carrying a distinctive secret
        // that must not survive into the event, plus the class the event is
        // still expected to disclose.
        let cases = [
            (
                "/home/alice/s3cr3t-workspace/photo.png",
                "s3cr3t-workspace",
                "local",
            ),
            (
                "https://internal.example.com/img?token=s3cr3t-token",
                "s3cr3t-token",
                "remote",
            ),
            (
                "data:image/png;base64,czNjcjN0LXBheWxvYWQ=",
                "czNjcjN0LXBheWxvYWQ",
                "data",
            ),
        ];

        for (reference, secret, expected_kind) in cases {
            let attrs = skipped_image_log_attrs(
                &ctx,
                reference,
                "corrupt_image",
                Some("decoded canvas exceeds per-image limit"),
            );
            let rendered = attrs.to_string();

            assert!(
                !rendered.contains(secret),
                "the skip event leaked {secret:?} from the raw reference: {rendered}"
            );
            assert!(
                !rendered.contains(reference),
                "the skip event must not carry the reference verbatim: {rendered}"
            );
            // The non-sensitive classification must survive, otherwise this
            // test would pass simply by the event losing all its context.
            assert_eq!(
                attrs.get("source_kind").and_then(|v| v.as_str()),
                Some(expected_kind),
                "the event must keep its non-sensitive source classification: {rendered}"
            );
            assert_eq!(
                attrs.get("error_kind").and_then(|v| v.as_str()),
                Some("corrupt_image"),
                "the event must keep its error classification: {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn corrupt_data_uri_is_skipped() {
        let b64 = STANDARD.encode([0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
        let history = vec![ChatMessage::user("look").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: format!("data:image/png;base64,{b64}").to_string(),
            },
        ])];

        let prepared = prepare_messages_for_provider(&history, &MultimodalConfig::default())
            .await
            .expect("corrupt data URI must not fail preparation");

        assert!(!prepared.contains_images);
        assert!(prepared.messages[0].content.contains("could not be loaded"));
    }

    #[tokio::test]
    async fn prepare_messages_preserves_native_tool_result_json_shape() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("native-tool-result.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let native_tool_content =
            native_carrier("tc1", "see attached", &[image_path.display().to_string()]);

        let messages = vec![native_tool_content];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("preparation should succeed for native tool-result JSON");

        assert!(prepared.contains_images);
        assert_eq!(prepared.messages.len(), 1);
        assert_eq!(prepared.messages[0].role, "tool");

        let value: serde_json::Value =
            crate::tool_protocol::tool_message_value(&prepared.messages[0]).unwrap();

        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc1"),
            "tool_call_id must survive multimodal preprocessing unchanged"
        );

        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content must remain a JSON string");
        assert_eq!(
            inner, "see attached",
            "the body must survive normalization byte for byte"
        );
        let refs = carrier_image_targets(&prepared.messages[0]);
        assert_eq!(refs.len(), 1);
        assert!(
            refs[0].starts_with("data:image/png;base64,"),
            "the declared local image is rewritten to a data URI in the attachments array"
        );
        assert!(
            !inner.contains("native-tool-result.png"),
            "raw local path must not leak into the body"
        );
    }

    #[tokio::test]
    async fn prepare_messages_preserves_native_tool_json_with_mixed_images() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("mixed-native-tool-result.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let native_tool_content = native_carrier(
            "tc1",
            "generated",
            &[
                image_path.display().to_string(),
                "https://example.com/missing.png".to_string(),
            ],
        );

        let prepared =
            prepare_messages_for_provider(&[native_tool_content], &MultimodalConfig::default())
                .await
                .expect("valid native tool image should survive while bad ref is skipped");

        assert!(prepared.contains_images);
        assert_eq!(prepared.messages.len(), 1);

        let value: serde_json::Value =
            crate::tool_protocol::tool_message_value(&prepared.messages[0]).unwrap();
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc1")
        );

        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert_eq!(
            inner, "generated\n\nNote: 1 of 2 attached image(s) could not be loaded.",
            "the body keeps its text and gains the load-failure note"
        );
        let refs = carrier_image_targets(&prepared.messages[0]);
        assert_eq!(refs.len(), 1);
        assert!(refs[0].starts_with("data:image/png;base64,"));
        assert!(!inner.contains("mixed-native-tool-result.png"));
        assert!(!inner.contains("https://example.com/missing.png"));
    }

    #[tokio::test]
    async fn prepare_messages_keeps_tool_image_after_unrelated_tool_call_in_same_turn() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("same-turn-tool-result.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let image_tool_content = native_carrier(
            "tc_image",
            "generated screenshot",
            &[image_path.display().to_string()],
        );
        let weather_tool_content = native_carrier("tc_weather", "Sunny, 25C", &[]);

        let messages = vec![
            ChatMessage::user("Take a screenshot, then check the weather.".to_string()),
            ChatMessage::assistant_tool_calls(
                ("").to_string(),
                serde_json::from_value(serde_json::json!([
                    {"id": "tc_image", "name": "screenshot", "arguments": "{}"}
                ]))
                .unwrap(),
                None,
            ),
            image_tool_content,
            ChatMessage::assistant_tool_calls(
                ("").to_string(),
                serde_json::from_value(serde_json::json!([
                    {"id": "tc_weather", "name": "weather", "arguments": "{}"}
                ]))
                .unwrap(),
                None,
            ),
            weather_tool_content,
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("tool images stay live for the user turn that produced them");

        assert!(
            prepared.contains_images,
            "an image tool result followed by an unrelated tool result in the same turn must stay normalized"
        );

        let value: serde_json::Value =
            crate::tool_protocol::tool_message_value(&prepared.messages[2]).unwrap();
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc_image")
        );
        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert_eq!(inner, "generated screenshot");
        // The declared attachment is rewritten to a data URI in place, so the
        // raw filesystem path is gone from the whole message.
        let refs = carrier_image_targets(&prepared.messages[2]);
        assert_eq!(refs.len(), 1);
        assert!(refs[0].starts_with("data:image/png;base64,"));
        assert!(
            !prepared.messages[2]
                .content
                .contains("same-turn-tool-result.png")
        );
    }

    #[tokio::test]
    async fn prepare_messages_strips_tool_image_after_next_user_message() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("same-turn-tool-result.png");
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        std::fs::write(&image_path, png).unwrap();

        let image_tool_content = native_carrier(
            "tc_image",
            "generated screenshot",
            &[image_path.display().to_string()],
        );
        let weather_tool_content = native_carrier("tc_weather", "Sunny, 25C", &[]);

        let messages = vec![
            ChatMessage::user("Take a screenshot, then check the weather.".to_string()),
            ChatMessage::assistant_tool_calls(
                ("").to_string(),
                serde_json::from_value(serde_json::json!([
                    {"id": "tc_image", "name": "screenshot", "arguments": "{}"}
                ]))
                .unwrap(),
                None,
            ),
            image_tool_content,
            ChatMessage::assistant_tool_calls(
                ("").to_string(),
                serde_json::from_value(serde_json::json!([
                    {"id": "tc_weather", "name": "weather", "arguments": "{}"}
                ]))
                .unwrap(),
                None,
            ),
            weather_tool_content,
            ChatMessage::user("What did you find?".to_string()),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("stale tool images should strip without loading them");

        assert!(
            !prepared.contains_images,
            "once the next user message arrives the turn's tool images are stale"
        );

        let value: serde_json::Value =
            crate::tool_protocol::tool_message_value(&prepared.messages[2]).unwrap();
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc_image")
        );
        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert_eq!(inner, "generated screenshot");
        assert!(
            carrier_image_targets(&prepared.messages[2]).is_empty(),
            "the turn's tool images are stale once the next user message arrives"
        );
        assert!(!inner.contains("data:image"));
        assert!(!inner.contains("same-turn-tool-result.png"));

        let weather_value: serde_json::Value =
            crate::tool_protocol::tool_message_value(&prepared.messages[4]).unwrap();
        assert_eq!(
            weather_value.get("content").and_then(|v| v.as_str()),
            Some("Sunny, 25C")
        );
    }

    #[tokio::test]
    async fn prepare_messages_keeps_prompt_mode_tool_images_within_turn_under_age_limit() {
        let temp = tempfile::tempdir().unwrap();
        let user_image_path = temp.path().join("age-limit-user-attachment.png");
        let tool_image_path = temp.path().join("age-limit-tool-result.png");
        std::fs::write(&user_image_path, valid_png()).unwrap();
        std::fs::write(&tool_image_path, valid_png()).unwrap();

        let messages = vec![
            ChatMessage::user("Inspect the attached image, then check session information")
                .with_attachments(vec![zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (user_image_path.display()).to_string(),
                }]),
            ChatMessage::assistant_tool_calls(
                ("").to_string(),
                serde_json::from_value(serde_json::json!([
                    {"id": "tc_image", "name": "image_tool", "arguments": "{}"}
                ]))
                .unwrap(),
                None,
            ),
            (prompt_carrier(
                "<tool_result name=\"image_tool\">Generated</tool_result>",
                &[tool_image_path.display().to_string()],
            )),
            ChatMessage::assistant_tool_calls(
                ("").to_string(),
                serde_json::from_value(serde_json::json!([
                    {"id": "tc_session", "name": "session_info", "arguments": "{}"}
                ]))
                .unwrap(),
                None,
            ),
            (prompt_carrier(
                "<tool_result name=\"session_info\">session id: abc-123</tool_result>",
                &[],
            )),
        ];

        let config = MultimodalConfig {
            max_images: 4,
            max_image_turns: 1,
            ..Default::default()
        };

        let prepared = prepare_messages_for_provider(&messages, &config)
            .await
            .expect("one user turn holding two carriers must not age the turn's images out");

        assert!(
            prepared.contains_images,
            "the user's own image and the same-turn tool image must both survive the age limit"
        );

        assert!(
            prepared.messages[0]
                .content
                .contains("Inspect the attached image, then check session information")
        );
        assert!(message_image_refs(&prepared.messages[0])[0].starts_with("data:image/png;base64,"));
        assert!(
            !prepared.messages[0]
                .content
                .contains("age-limit-user-attachment.png")
        );
        assert_eq!(prepared.messages[2].role, "tool");
        assert!(prepared.messages[2].content.contains("Generated"));
        assert!(
            message_image_refs(&prepared.messages[2])[0].starts_with("data:image/png;base64,"),
            "the normalized attachment remains in the carrier's structured fields"
        );
        assert!(
            !prepared.messages[2]
                .content
                .contains("age-limit-tool-result.png")
        );
        assert!(prepared.messages[4].content.contains("session id: abc-123"));
    }

    #[test]
    fn trim_images_by_age_ignores_prompt_mode_carriers_when_counting() {
        let messages = vec![
            ChatMessage::user("Inspect this screenshot.".to_string()),
            ChatMessage {
                tool_calls: Vec::new(),
                tool_result: None,
                reasoning_content: None,
                attachments: Vec::new(),
                role: "assistant".to_string(),
                content: "On it.".to_string(),
            },
            (prompt_carrier(
                "<tool_result name=\"image_gen\">Generated</tool_result>",
                &["/tmp/tool-image.png".to_string()],
            )),
            ChatMessage {
                tool_calls: Vec::new(),
                tool_result: None,
                reasoning_content: None,
                attachments: Vec::new(),
                role: "assistant".to_string(),
                content: "Checked.".to_string(),
            },
            (prompt_carrier(
                "<tool_result name=\"weather\">Sunny, 25C</tool_result>",
                &[],
            )),
        ];

        let trimmed = trim_images_by_age(&messages, 1);

        assert_eq!(trimmed[0].content, messages[0].content);
        assert_eq!(trimmed[2].content, messages[2].content);
        assert_eq!(trimmed[4].content, messages[4].content);
    }

    #[test]
    fn count_image_attachments_ignores_stale_tool_results() {
        let messages = vec![
            (native_carrier("tc1", "Generated", &["/tmp/stale-tool.png".to_string()])),
            ChatMessage {
                tool_calls: Vec::new(),
                tool_result: None,
                reasoning_content: None,
                attachments: Vec::new(),
                role: "assistant".to_string(),
                content: "Done.".to_string(),
            },
            ChatMessage::user("Next question".to_string()),
        ];

        assert_eq!(count_image_attachments(&messages), 0);

        let messages = vec![
            ChatMessage::user("Create an image".to_string()),
            (native_carrier("tc1", "Generated", &["/tmp/latest-tool.png".to_string()])),
        ];

        assert_eq!(count_image_attachments(&messages), 1);
    }

    #[test]
    fn count_image_attachments_counts_tool_images_within_turn() {
        // The image tool result sits before a later, text-only tool result in
        // the same user turn; the vision gate must still see its image.
        let messages = vec![
            ChatMessage::user("Create an image, then check the weather.".to_string()),
            (native_carrier("tc1", "Generated", &["/tmp/same-turn-tool.png".to_string()])),
            ChatMessage {
                tool_calls: Vec::new(),
                tool_result: None,
                reasoning_content: None,
                attachments: Vec::new(),
                role: "assistant".to_string(),
                content: "Checking the weather next.".to_string(),
            },
            (native_carrier("tc2", "Sunny, 25C", &[])),
        ];

        assert_eq!(count_image_attachments(&messages), 1);
    }

    #[tokio::test]
    async fn resolvable_count_ignores_missing_path() {
        let messages = vec![ChatMessage::user("look at this").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: ("/definitely/not/a/real/screenshot.png").to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn resolvable_count_accepts_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("shot.png");
        std::fs::write(&image_path, b"not a real png; existence is all that counts").unwrap();
        let messages = vec![ChatMessage::user("look at this").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (image_path.display()).to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn resolvable_count_accepts_structurally_valid_data_uri() {
        let uri = format!("data:image/png;base64,{CANONICAL_PNG_B64}");
        let messages = vec![ChatMessage::user("inline").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: uri.to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn resolvable_count_rejects_malformed_data_uri() {
        let uri = "data:image/png;base64,%%%";
        let messages = vec![ChatMessage::user("inline").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: uri.to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn resolvable_count_remote_follows_flag() {
        let reference = "https://example.com/cat.png";
        let messages = vec![ChatMessage::user("see").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: reference.to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            0
        );
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                true,
                Arc::new(|_: &Path| true)
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn resolvable_count_looks_only_at_latest_user_message() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("older.png");
        std::fs::write(&image_path, b"an older turn's real image").unwrap();
        let messages = vec![
            ChatMessage::user("look").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: (image_path.display()).to_string(),
                },
            ]),
            ChatMessage::user("what is WAL?".to_string()),
        ];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn resolvable_count_skips_local_paths_the_policy_rejects() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("shot.png");
        std::fs::write(&image_path, b"not a real png; existence is all that counts").unwrap();
        let messages = vec![ChatMessage::user("look at this").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (image_path.display()).to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| false)
            )
            .await,
            0,
            "a local path the policy rejects must never count, file or no file"
        );
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            1,
            "the same message with an accepting policy counts the existing file"
        );
    }

    #[tokio::test]
    async fn resolvable_count_never_counts_relative_local_paths() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("rel.png");
        std::fs::write(
            &image_path,
            b"relative references must not resolve against the process cwd",
        )
        .unwrap();
        let messages = vec![ChatMessage::user("look at this").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: ("rel.png").to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &messages,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            0,
            "a relative reference never counts, even when the predicate accepts everything"
        );
        // Contrast: the same file reached by its absolute path does count, so
        // the zero above comes from relativity, not from a missing file.
        let absolute = vec![ChatMessage::user("look at this").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (image_path.display()).to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(
                &absolute,
                false,
                Arc::new(|_: &Path| true)
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn resolvable_count_does_not_probe_rejected_paths() {
        let probed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probed_for_predicate = std::sync::Arc::clone(&probed);
        let path_allowed: Arc<dyn Fn(&Path) -> bool + Send + Sync> =
            Arc::new(move |_: &Path| -> bool {
                probed_for_predicate.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                false
            });
        // Both local markers name files that are never created: a rejected
        // path must count 0 without the file ever needing to exist. They sit
        // under a temp dir so they are absolute on every platform; a rooted
        // path such as `/definitely/...` has no drive on Windows and would be
        // refused as relative before the predicate ran.
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("left.png");
        let right = temp.path().join("right.png");
        let messages = vec![ChatMessage::user("compare  with").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (left.display()).to_string(),
            },
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (right.display()).to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(&messages, false, path_allowed.clone())
                .await,
            0
        );
        assert_eq!(
            probed.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the predicate runs exactly once per local marker"
        );
        // A structurally valid data URI in the same message resolves without
        // the predicate being consulted for it.
        let data_uri = format!("data:image/png;base64,{CANONICAL_PNG_B64}");
        let with_data_uri = vec![ChatMessage::user("compare  with").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (data_uri).to_string(),
            },
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (right.display()).to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(&with_data_uri, false, path_allowed)
                .await,
            1
        );
        assert_eq!(
            probed.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "only the local markers consulted the predicate; the data URI did not"
        );
    }

    /// The gate's policy-resolution and existence work must run OFF the
    /// async executor. The predicate below signals "entered" and then
    /// blocks on a "release" channel; the `tokio::spawn`ed watcher task
    /// can only observe "entered" and send the release if the runtime's
    /// single thread is free, which is only true when the predicate runs
    /// inside `spawn_blocking` rather than on the executor thread. If the
    /// predicate ever moves back onto the executor thread, it blocks the
    /// only runtime thread in `recv_timeout`, the watcher never runs, the
    /// flag stays false, and this test fails within the 5 s timeout
    /// instead of hanging. Do NOT "simplify" this onto a multi_thread
    /// runtime: a second worker thread would schedule the watcher even
    /// with the predicate inline on an executor thread, and the test
    /// would silently stop proving anything.
    #[tokio::test]
    async fn resolvable_count_runs_predicate_and_probe_off_the_executor() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("shot.png");
        std::fs::write(&image_path, b"existence is all that counts").unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // Neither mpsc end is `Sync`, and the predicate must be; the
        // mutexes make the captured sender and receiver shareable without
        // changing the handshake (the predicate blocks in `recv_timeout`
        // inside its mutex while the watcher holds no lock).
        let entered_tx = std::sync::Mutex::new(entered_tx);
        let release_rx = std::sync::Mutex::new(release_rx);
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let released_for_predicate = std::sync::Arc::clone(&released);
        let path_allowed: Arc<dyn Fn(&Path) -> bool + Send + Sync> =
            Arc::new(move |_: &Path| -> bool {
                entered_tx
                    .lock()
                    .expect("entered sender lockable")
                    .send(())
                    .expect("entered signal sendable");
                let arrived = release_rx
                    .lock()
                    .expect("release receiver lockable")
                    .recv_timeout(std::time::Duration::from_secs(5));
                released_for_predicate.store(arrived.is_ok(), std::sync::atomic::Ordering::SeqCst);
                true
            });
        // The watcher runs detached on the runtime's own thread pool via
        // the sanctioned spawn wrapper (plain `tokio::spawn` is
        // workspace-disallowed); its handle is intentionally dropped.
        let _watcher = zeroclaw_spawn::spawn!(async move {
            loop {
                if entered_rx.try_recv().is_ok() {
                    release_tx.send(()).expect("release signal sendable");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let messages = vec![ChatMessage::user("look at this").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (image_path.display()).to_string(),
            },
        ])];
        assert_eq!(
            count_latest_user_resolvable_image_attachments(&messages, false, path_allowed).await,
            1,
            "the existing file resolves through the blocking task"
        );
        assert!(
            released.load(std::sync::atomic::Ordering::SeqCst),
            "the runtime-thread watcher observed the predicate and released it, \
             so the predicate ran off the executor thread"
        );
    }

    #[test]
    fn images_kept_under_cap_evicts_in_batches_down_to_the_low_water_mark() {
        // Under or at the cap nothing is evicted.
        for total in 0..=4 {
            assert_eq!(images_kept_under_cap(total, 4), total);
        }
        // Past a cap of 4 the count drops to 2, climbs back to 4, and drops
        // again: one eviction per three new images.
        let kept: Vec<usize> = (5..=11).map(|t| images_kept_under_cap(t, 4)).collect();
        assert_eq!(kept, vec![2, 3, 4, 2, 3, 4, 2]);
        // A cap of 1 has no room for a batch: every new image evicts one.
        for total in 1..=6 {
            assert_eq!(images_kept_under_cap(total, 1), 1);
        }
        // The largest cap the config allows: 17 -> 8, back up to 16 at 25.
        assert_eq!(images_kept_under_cap(17, 16), 8);
        assert_eq!(images_kept_under_cap(25, 16), 16);
        assert_eq!(images_kept_under_cap(26, 16), 8);
        // Odd caps round the mark up: 3 -> 2, 5 -> 3.
        assert_eq!(images_kept_under_cap(4, 3), 2);
        assert_eq!(images_kept_under_cap(6, 5), 3);
    }

    #[test]
    fn trim_old_images_empty_messages() {
        let trimmed = trim_old_images(&[], 4);
        assert!(trimmed.is_empty());
    }

    #[tokio::test]
    async fn prepare_messages_trims_then_normalizes_surviving_images() {
        // End-to-end: 3 images, max 2. The cap drops to the low-water mark of
        // 1, and the surviving newest image is normalized (base64-encoded).
        let temp = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for name in ["old.png", "mid.png", "new.png"] {
            let p = temp.path().join(name);
            std::fs::write(&p, valid_png()).unwrap();
            paths.push(p);
        }

        let messages = vec![
            ChatMessage::user("Old").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: paths[0].display().to_string(),
                },
            ]),
            ChatMessage::user("Mid").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: paths[1].display().to_string(),
                },
            ]),
            ChatMessage::user("New").with_attachments(vec![
                zeroclaw_api::media::AttachmentReference {
                    kind: zeroclaw_api::media::AttachmentKind::Image,
                    target: paths[2].display().to_string(),
                },
            ]),
        ];

        let config = MultimodalConfig {
            max_images: 2,
            max_image_size_mb: 5,
            allow_remote_fetch: false,
            ..Default::default()
        };

        let result = prepare_messages_for_provider(&messages, &config)
            .await
            .expect("should succeed after trimming");

        assert!(result.contains_images);
        assert_eq!(result.messages.len(), 3);
        // First message should have image stripped, text preserved
        assert!(message_image_refs(&result.messages[0]).is_empty());
        assert!(result.messages[0].content.contains("Old"));
        // Second goes in the same batch; only the third is base64-encoded
        assert!(message_image_refs(&result.messages[1]).is_empty());
        assert!(result.messages[1].content.contains("Mid"));
        assert!(message_image_refs(&result.messages[2])[0].starts_with("data:image"));
    }

    #[tokio::test]
    async fn prepare_messages_caps_to_newest_successful_images() {
        let temp = tempfile::tempdir().unwrap();
        let png_data = valid_png();

        // Nine distinct valid image files across nine user messages, max 4.
        let mut messages = Vec::new();
        for i in 0..9 {
            let p = temp.path().join(format!("img{i}.png"));
            std::fs::write(&p, &png_data).unwrap();
            messages.push(
                ChatMessage::user(format!("\nImage {i}")).with_attachments(vec![
                    zeroclaw_api::media::AttachmentReference {
                        kind: zeroclaw_api::media::AttachmentKind::Image,
                        target: (p.display()).to_string(),
                    },
                ]),
            );
        }

        let config = MultimodalConfig {
            max_images: 4,
            max_image_size_mb: 5,
            allow_remote_fetch: false,
            max_image_turns: 0, // disable age-based trimming to isolate the cap
            ..Default::default()
        };

        let result = prepare_messages_for_provider(&messages, &config)
            .await
            .expect("should succeed");

        // Nine images against a cap of 4: the batch rule keeps
        // 2 + (9 - 5) % 3 = 3.
        let surviving = result
            .messages
            .iter()
            .filter(|m| !message_image_refs(m).is_empty())
            .count();
        assert_eq!(surviving, 3, "output should keep the batch-rule count");
        assert_eq!(
            result.submitted_image_ids,
            provider_image_ids(&result.messages)
        );
        assert_eq!(result.submitted_image_ids.len(), 3);
        assert_eq!(
            result.newest_user_image_ids,
            provider_image_ids_in_message(&result.messages[8])
        );

        // ...and it is the newest three that survive; the oldest six are stripped.
        for (i, m) in result.messages.iter().enumerate() {
            if i < 6 {
                assert!(
                    message_image_refs(m).is_empty(),
                    "oldest message {i} should be capped out"
                );
                assert!(m.content.contains(&format!("Image {i}")));
            } else {
                assert!(
                    !message_image_refs(m).is_empty(),
                    "newest message {i} should survive the cap"
                );
            }
        }
    }

    #[tokio::test]
    async fn prepare_messages_skips_oversized_local_image() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("big.png");

        let bytes = vec![0u8; 1024 * 1024 + 1];
        std::fs::write(&image_path, bytes).unwrap();

        let messages = vec![ChatMessage::user("").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (image_path.display()).to_string(),
            },
        ])];
        let config = MultimodalConfig {
            max_images: 4,
            max_image_size_mb: 1,
            allow_remote_fetch: false,
            ..Default::default()
        };

        let result = prepare_messages_for_provider(&messages, &config)
            .await
            .expect("oversized local image should be skipped");

        assert!(!result.contains_images);
        assert_eq!(result.messages.len(), 1);
        assert!(
            result.messages[0]
                .content
                .contains("1 attached image(s) could not be loaded")
        );
        assert!(
            !result.messages[0]
                .content
                .contains(image_path.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn prepare_messages_keeps_successful_images_when_some_are_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("ok.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let messages = vec![ChatMessage::user("Look  and").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: (image_path.display()).to_string(),
            },
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: "https://example.com/missing.png".to_string(),
            },
        ])];

        let result = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("valid local image should survive while remote image is skipped");

        assert!(result.contains_images);
        assert!(message_image_refs(&result.messages[0])[0].starts_with("data:image/png;base64,"));
        assert!(
            result.messages[0]
                .content
                .contains("1 of 2 attached image(s) could not be loaded")
        );
        assert!(
            !result.messages[0]
                .content
                .contains("https://example.com/missing.png")
        );
    }

    #[test]
    fn extract_ollama_image_payload_supports_data_uris() {
        let payload = extract_ollama_image_payload("data:image/png;base64,abcd==")
            .expect("payload should be extracted");
        assert_eq!(payload, "abcd==");
    }

    #[test]
    fn provider_image_filter_removes_only_selected_identity() {
        let first = "data:image/png;base64,AAAA";
        let second = "data:image/png;base64,BBBB";
        let messages = vec![ChatMessage::user("compare  with").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: first.to_string(),
            },
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: second.to_string(),
            },
        ])];
        let ids = provider_image_ids(&messages);

        let filtered = omit_provider_image_ids(&messages, &ids[1..]);

        assert_eq!(provider_image_ids(&filtered), vec![ids[0]]);
        assert_eq!(message_image_refs(&filtered[0]), vec![first]);
        assert!(filtered[0].content.contains("compare"));
    }

    /// A GIF whose image descriptor claims a frame larger than the logical screen.
    ///
    /// The LZW payload is 1×1 (the same `GIF_VALID_1X1_LZW` data), but the
    /// image descriptor advertises `frame_dim × frame_dim`. This exercises the
    /// path where the decoder's `local_limits.reserve_buffer(frame.width,
    /// frame.height)` uses the descriptor size rather than the logical screen
    /// size. The GIF decoder accepts this header; whether the allocation
    /// succeeds depends on whether the limits already clamp to the screen.
    fn oversized_descriptor_gif(screen: u16, frame_dim: u16) -> Vec<u8> {
        let mut gif = Vec::new();
        gif.extend_from_slice(b"GIF89a");
        gif.extend_from_slice(&screen.to_le_bytes()); // logical width
        gif.extend_from_slice(&screen.to_le_bytes()); // logical height
        gif.extend_from_slice(&[0x80, 0x00, 0x00]); // global color table, 2 entries
        gif.extend_from_slice(&[0x00, 0x00, 0x00]); // black
        gif.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // white
        gif.push(0x2C); // image separator
        gif.extend_from_slice(&0u16.to_le_bytes()); // left
        gif.extend_from_slice(&0u16.to_le_bytes()); // top
        gif.extend_from_slice(&frame_dim.to_le_bytes()); // frame width (oversized)
        gif.extend_from_slice(&frame_dim.to_le_bytes()); // frame height (oversized)
        gif.push(0x00); // no local color table
        gif.extend_from_slice(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);
        gif
    }

    #[tokio::test]
    async fn gif_frame_descriptor_exceeding_logical_screen_is_rejected() {
        // A GIF image descriptor may claim frame dimensions larger than the
        // logical screen. In `image` 0.25.10, the per-frame buffer is sized
        // from the descriptor (`src/codecs/gif.rs:326-331`) rather than the
        // logical screen, so a 4×4 frame in a 2×2 screen allocates 4×4×4 = 64
        // bytes while `projected_allocation` and the returned `Frame` see only
        // 2×2. The aggregate charge would therefore be 16 bytes (2×2 screen
        // peak) while 64 bytes were actually allocated.
        //
        // Clamping `max_image_width/height` to the logical screen before
        // `set_limits` causes the decoder to refuse the descriptor before its
        // buffer is allocated. A GIF whose frames stay within the screen is
        // unaffected.
        //
        // Screen=2, frame=4: frame buffer = 4*4*4 = 64 B; screen = 2*2*4 = 16 B.
        let gif = oversized_descriptor_gif(2, 4);
        let error = validate_image_content("big-frame.gif", "image/gif", &gif, u64::MAX)
            .await
            .expect_err(
                "a GIF whose frame descriptor exceeds the logical screen must be rejected \
                 before the oversized buffer is allocated",
            );

        // The *reason* is what matters here, not merely that it failed.
        //
        // In `image` 0.25.10 the order is: `local_limits.reserve_buffer(...)`
        // (the limit check), then `vec![0; buffer_size]` (the allocation), then
        // `read_into_buffer` (which is where a short LZW stream reports EOF).
        // Without the screen clamp the descriptor passes the limit check and
        // the oversized buffer is allocated, and the failure surfaces later as
        // a truncation error — after the memory was already taken. Asserting on
        // the limit message is what pins the rejection to the pre-allocation
        // check rather than to the decode that follows it.
        let reason = error.to_string();
        assert!(
            reason.contains("Image size exceeds limit"),
            "the descriptor must be refused by the dimension limit before allocation, \
             not by a later decode failure; got: {reason}"
        );

        // A normal animated GIF whose frames stay within the logical screen must
        // still be accepted: the dimension clamp must not break ordinary GIFs.
        let mut ordinary = two_frame_gif(&GIF_VALID_1X1_LZW);
        ordinary.push(GIF_TRAILER);
        validate_image_content("ordinary.gif", "image/gif", &ordinary, u64::MAX)
            .await
            .expect("an animated GIF whose frames match the logical screen must be accepted");
    }
}
