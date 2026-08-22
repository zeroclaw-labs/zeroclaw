use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Client;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_config::schema::{MultimodalConfig, build_runtime_proxy_client_with_timeouts};

const IMAGE_MARKER_PREFIX: &str = "[IMAGE:";
const ALLOWED_IMAGE_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

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
}

/// Records one decode against the ambient counter, if a test installed one.
#[cfg(test)]
fn record_decode_call() {
    let _ = DECODE_CALLS.try_with(|calls| {
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
}

const LOCAL_IMAGE_CACHE_MAX_ENTRIES: usize = 32;
const LOCAL_IMAGE_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct PreparedMessages {
    pub messages: Vec<ChatMessage>,
    pub contains_images: bool,
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

    #[error("invalid multimodal image marker '{input}': {reason}")]
    InvalidMarker { input: String, reason: String },

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
    /// Media type outside [`ALLOWED_IMAGE_MIME_TYPES`].
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
    if !ALLOWED_IMAGE_MIME_TYPES
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

fn is_loadable_image_reference(candidate: &str) -> bool {
    candidate.starts_with('/')
        || candidate.starts_with("http://")
        || candidate.starts_with("https://")
        || candidate.starts_with("data:")
        || is_windows_path(candidate)
        || is_windows_unc_path(candidate)
}

/// Returns true for Windows-style absolute paths like `C:\…` or `D:/…`.
fn is_windows_path(candidate: &str) -> bool {
    let mut chars = candidate.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let Some(second) = chars.next() else {
        return false;
    };
    if second != ':' {
        return false;
    }
    matches!(chars.next(), Some('\\') | Some('/'))
}

fn is_windows_unc_path(candidate: &str) -> bool {
    let Some(rest) = candidate.strip_prefix(r"\\") else {
        return false;
    };
    if rest.starts_with('?') || rest.starts_with('.') {
        return false;
    }
    let mut parts = rest.splitn(2, ['\\', '/']);
    let server = parts.next().unwrap_or("");
    let share = parts.next().unwrap_or("");
    !server.is_empty() && !share.is_empty()
}

fn collapse_wrapped_marker(raw: &str) -> String {
    if !raw.contains('\n') && !raw.contains('\r') {
        return raw.trim().to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut skip_ws = false;
    for ch in raw.chars() {
        if ch == '\n' || ch == '\r' {
            skip_ws = true;
            continue;
        }
        if skip_ws {
            if ch.is_whitespace() {
                continue;
            }
            skip_ws = false;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

/// True when `content` holds an image marker, terminated or not.
///
/// This is how a provider adapter tells *residue of this crate's own marker
/// normalization* from a data URI the author wrote deliberately. An
/// unterminated marker is copied through by [`parse_image_markers`] verbatim,
/// prefix included, so the prefix is present in both the input and the cleaned
/// output whenever residue is possible.
pub(crate) fn carries_image_marker(content: &str) -> bool {
    content.contains(IMAGE_MARKER_PREFIX)
}

pub fn parse_image_markers(content: &str) -> (String, Vec<String>) {
    let mut refs = Vec::new();
    let mut cleaned = String::with_capacity(content.len());
    let mut cursor = 0usize;

    while let Some(rel_start) = content[cursor..].find(IMAGE_MARKER_PREFIX) {
        let start = cursor + rel_start;
        cleaned.push_str(&content[cursor..start]);

        let marker_start = start + IMAGE_MARKER_PREFIX.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            cleaned.push_str(&content[start..]);
            cursor = content.len();
            break;
        };

        let end = marker_start + rel_end;
        let candidate = collapse_wrapped_marker(&content[marker_start..end]);

        if candidate.is_empty() || !is_loadable_image_reference(&candidate) {
            // Preserve the original marker text (placeholders like
            // `[IMAGE:...]` or `[IMAGE:<path>]` should survive as prose
            // rather than triggering a loader error).
            cleaned.push_str(&content[start..=end]);
        } else {
            refs.push(candidate);
        }

        cursor = end + 1;
    }

    if cursor < content.len() {
        cleaned.push_str(&content[cursor..]);
    }

    (cleaned.trim().to_string(), refs)
}

pub fn count_image_markers(messages: &[ChatMessage]) -> usize {
    let latest_tool_indices = latest_tool_result_indices(messages);
    count_image_markers_with_latest_tool_results(messages, &latest_tool_indices)
}

fn count_image_markers_with_latest_tool_results(
    messages: &[ChatMessage],
    latest_tool_result_indices: &HashSet<usize>,
) -> usize {
    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            should_normalize_message_images(*index, message, latest_tool_result_indices)
        })
        .map(|(_, message)| parse_image_markers(&message.content).1.len())
        .sum()
}

pub fn contains_image_markers(messages: &[ChatMessage]) -> bool {
    count_image_markers(messages) > 0
}

pub fn count_user_image_markers(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|message| message.role == "user" && !is_prompt_tool_result_message(message))
        .map(|message| parse_image_markers(&message.content).1.len())
        .sum()
}

pub fn count_latest_user_image_markers(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "user" && !is_prompt_tool_result_message(message))
        .map(|message| parse_image_markers(&message.content).1.len())
        .unwrap_or(0)
}

/// Media-marker kinds this module recognizes. `IMAGE` is the only kind
/// resolved into provider content parts; [`AUDIO_MARKER_KINDS`] is the strict
/// subset degraded when a loadable payload would otherwise reach the model as
/// literal text. Both marker regexes below derive their kind alternation from
/// these consts so the strip-all and strip-audio paths cannot drift apart on
/// which kinds exist. The channel grammar (`ATTACHMENT_KINDS` in
/// `crates/zeroclaw-channels/src/util.rs`) recognizes these same kinds plus
/// `LOCATION`, which carries coordinates rather than a file reference and has
/// no provider-side handling; the two lists live in different crates
/// deliberately (providers cannot depend on channels).
const MEDIA_MARKER_KINDS: &[&str] = &[
    "IMAGE", "PHOTO", "DOCUMENT", "FILE", "VIDEO", "VOICE", "AUDIO",
];

/// Marker kinds whose loadable payload must not stay model-visible. No
/// provider resolves audio into content parts, and an audio path is not
/// otherwise actionable by the model: asked what it hears, a model handed a
/// bare path tends to fabricate having played the file. Every other kind in
/// [`MEDIA_MARKER_KINDS`] keeps its payload — `IMAGE` is resolved for vision
/// downstream, and `PHOTO`/`DOCUMENT`/`FILE`/`VIDEO` paths stay actionable
/// (file tools read them, and the channel delivery contract has the model
/// copy them into outbound reply markers), so stripping those would break
/// document and file delivery.
const AUDIO_MARKER_KINDS: &[&str] = &["VOICE", "AUDIO"];

pub fn strip_media_markers(text: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(
            r"(?i)\[(?:{}):[^\]]*\]",
            MEDIA_MARKER_KINDS.join("|")
        ))
        .unwrap()
    });
    RE.replace_all(text, "[media attachment]").into_owned()
}

/// Matches the audio-kind markers ([`AUDIO_MARKER_KINDS`]), capturing the
/// payload for the loadable-reference check.
static AUDIO_MARKER_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"(?i)\[(?:{}):([^\]]*)\]",
        AUDIO_MARKER_KINDS.join("|")
    ))
    .unwrap()
});

/// Replace audio markers (`[AUDIO:...]`, `[VOICE:...]`) whose payload is a
/// *loadable* reference (absolute path, `http(s)://` URL, or `data:` URI) with
/// the same `[media attachment]` placeholder the degrade path uses, returning
/// the rewritten text and the number of markers replaced.
///
/// Non-loadable payloads are left as literal text — placeholders (`[AUDIO:...]`),
/// prose (`[AUDIO:<clip>]`), and the no-transcription note (`[Audio: attached]`)
/// are harmless and must survive — mirroring how [`parse_image_markers`]
/// preserves non-loadable `[IMAGE:...]` markers. Runs over the raw string so it
/// also cleans a marker embedded in a native tool-result JSON blob
/// (`{"content":"…[AUDIO:/clip.wav]…"}`): `[media attachment]` contains no
/// JSON-special characters, so the surrounding object stays valid.
fn strip_unplayable_audio_markers(text: &str) -> (String, usize) {
    let mut stripped = 0usize;
    let out = AUDIO_MARKER_RE.replace_all(text, |caps: &regex::Captures<'_>| {
        let payload = collapse_wrapped_marker(&caps[1]);
        if !payload.is_empty() && is_loadable_image_reference(&payload) {
            stripped += 1;
            "[media attachment]".to_string()
        } else {
            // Preserve placeholder/prose markers verbatim.
            caps[0].to_string()
        }
    });
    (out.into_owned(), stripped)
}

/// Strip loadable audio markers (see `strip_unplayable_audio_markers`)
/// across every message in `messages`, logging one degradation warning when
/// any are removed. Returns the input borrowed when no candidate marker is
/// present (the common, allocation-free path) or an owned rebuilt vector
/// otherwise.
///
/// This is the shared seam keeping a raw audio path out of provider payloads,
/// whichever route the history takes:
/// - the main iteration prep ([`prepare_messages_for_provider`], via
///   `prepare_messages_inner`), and
/// - one-shot queries that dispatch history directly without full prep (the
///   max-iteration graceful summary and the other `run_model_query` callers).
///
/// Non-audio media markers pass through untouched; see `AUDIO_MARKER_KINDS`
/// for why the split falls where it does.
pub fn sanitize_audio_markers(messages: &[ChatMessage]) -> Cow<'_, [ChatMessage]> {
    if !messages
        .iter()
        .any(|m| AUDIO_MARKER_RE.is_match(&m.content))
    {
        return Cow::Borrowed(messages);
    }

    let mut stripped = 0usize;
    let rebuilt: Vec<ChatMessage> = messages
        .iter()
        .map(|m| {
            let (content, n) = strip_unplayable_audio_markers(&m.content);
            stripped += n;
            ChatMessage {
                role: m.role.clone(),
                content,
            }
        })
        .collect();

    if stripped > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "markers_stripped": stripped,
                })),
            "multimodal: stripped unplayable audio marker(s) (AUDIO/VOICE); no provider resolves audio into content parts, so a raw path/URL was replaced with a placeholder instead of being sent to the model as text"
        );
    }

    Cow::Owned(rebuilt)
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

pub(crate) fn is_prompt_tool_result_message(message: &ChatMessage) -> bool {
    message.role == "user" && message.content.trim_start().starts_with("[Tool results]")
}

fn is_tool_result_carrier(message: &ChatMessage) -> bool {
    message.role == "tool" || is_prompt_tool_result_message(message)
}

fn latest_tool_result_indices(messages: &[ChatMessage]) -> HashSet<usize> {
    let mut indices = HashSet::new();
    let Some((last_index, last_message)) = messages.iter().enumerate().next_back() else {
        return indices;
    };

    if is_prompt_tool_result_message(last_message) {
        indices.insert(last_index);
        return indices;
    }

    if last_message.role == "tool" {
        for (index, message) in messages.iter().enumerate().rev() {
            if message.role != "tool" {
                break;
            }
            indices.insert(index);
        }
    }

    indices
}

fn should_normalize_message_images(
    index: usize,
    message: &ChatMessage,
    latest_tool_result_indices: &HashSet<usize>,
) -> bool {
    if is_tool_result_carrier(message) {
        return latest_tool_result_indices.contains(&index);
    }

    message.role == "user"
}

fn stripped_image_marker_text(content: &str) -> String {
    let (cleaned, refs) = parse_image_markers(content);
    if refs.is_empty() {
        return content.to_string();
    }

    if cleaned.trim().is_empty() {
        "[image removed from history]".to_string()
    } else {
        cleaned
    }
}

fn strip_tool_result_image_markers(message: &ChatMessage) -> ChatMessage {
    if !message.content.contains(IMAGE_MARKER_PREFIX) {
        return message.clone();
    }

    if message.role == "tool"
        && let Ok(serde_json::Value::Object(mut obj)) =
            serde_json::from_str::<serde_json::Value>(&message.content)
        && let Some(serde_json::Value::String(inner)) = obj.get("content").cloned()
    {
        let stripped = stripped_image_marker_text(&inner);
        if stripped == inner {
            return message.clone();
        }

        obj.insert("content".to_string(), serde_json::Value::String(stripped));
        return ChatMessage {
            role: message.role.clone(),
            content: serde_json::Value::Object(obj).to_string(),
        };
    }

    ChatMessage {
        role: message.role.clone(),
        content: stripped_image_marker_text(&message.content),
    }
}

fn replay_message_without_stale_tool_images(
    index: usize,
    message: &ChatMessage,
    latest_tool_result_indices: &HashSet<usize>,
) -> ChatMessage {
    if is_tool_result_carrier(message) && !latest_tool_result_indices.contains(&index) {
        strip_tool_result_image_markers(message)
    } else {
        message.clone()
    }
}

async fn normalize_native_tool_result_json(
    content: &str,
    config: &MultimodalConfig,
    max_bytes: usize,
    remote_client: &Client,
    ctx: &ImageNormalizeCtx<'_>,
    cache: Option<&mut LocalImageCache>,
    remaining_budget: &mut u64,
) -> Option<(String, bool)> {
    let Ok(serde_json::Value::Object(mut obj)) = serde_json::from_str::<serde_json::Value>(content)
    else {
        return None;
    };

    let Some(serde_json::Value::String(inner)) = obj.get("content").cloned() else {
        return None;
    };

    let (cleaned_text, refs) = parse_image_markers(&inner);
    if refs.is_empty() {
        return None;
    }

    let normalized = normalize_image_references(
        &refs,
        config,
        max_bytes,
        remote_client,
        ctx,
        cache,
        remaining_budget,
    )
    .await;
    let new_inner = compose_multimodal_content(
        &cleaned_text,
        &normalized.data_uris,
        normalized.skipped_count,
        refs.len(),
    );
    obj.insert("content".to_string(), serde_json::Value::String(new_inner));

    Some((
        serde_json::Value::Object(obj).to_string(),
        !normalized.data_uris.is_empty(),
    ))
}

pub async fn prepare_messages_for_provider(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
) -> anyhow::Result<PreparedMessages> {
    prepare_messages_inner(messages, config, None).await
}

/// Like [`prepare_messages_for_provider`] but reuses a [`LocalImageCache`]
/// across calls so each unique local image file is read from disk at most
/// once per session (or once per modification for mutable files).
pub async fn prepare_messages_for_provider_cached(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
    cache: &mut LocalImageCache,
) -> anyhow::Result<PreparedMessages> {
    prepare_messages_inner(messages, config, Some(cache)).await
}

async fn prepare_messages_inner(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
    mut cache: Option<&mut LocalImageCache>,
) -> anyhow::Result<PreparedMessages> {
    // Strip loadable audio markers before any provider sees the history. Left
    // in place, an audio path reaches the model as literal text and fails
    // silently — the model typically hallucinates having played the file,
    // which is worse than an explicit degradation. `[IMAGE:...]` markers are
    // handled by the normalization below; other media kinds keep their
    // payloads for delivery. The shared seam borrows the input untouched when
    // no audio marker is present, so the common hot path stays allocation-free.
    let sanitized = sanitize_audio_markers(messages);
    let messages: &[ChatMessage] = &sanitized;

    let (max_images, max_image_size_mb) = config.effective_limits();
    let max_bytes = max_image_size_mb.saturating_mul(1024 * 1024);

    let latest_tool_indices = latest_tool_result_indices(messages);
    let total_images = count_image_markers_with_latest_tool_results(messages, &latest_tool_indices);

    if total_images == 0 {
        return Ok(PreparedMessages {
            messages: messages
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    replay_message_without_stale_tool_images(index, message, &latest_tool_indices)
                })
                .collect(),
            contains_images: false,
        });
    }

    // Apply per-request image cap and age trimming *before* normalization to
    // bound the number of candidates we decode. Full pixel validation is now
    // CPU and memory intensive (unlike the prior header-only sniff), so
    // decoding every candidate in a long history would create an unbounded
    // resource sink even though only `max_images` are sent to the provider.
    //
    // Trade-off: a failed newer image can now consume budget and evict an
    // older valid one. This is acceptable because (1) the aggregate decode
    // budget further bounds total work, (2) most images that pass the
    // encoded-byte check will decode successfully, and (3) preventing the
    // resource exhaustion hazard takes precedence over the marginal UX loss.
    let remote_client = build_runtime_proxy_client_with_timeouts("model_provider.ollama", 30, 10);
    let latest_tool_indices = latest_tool_result_indices(messages);

    // First pass: apply age-based trimming when configured.
    let age_trimmed = if config.max_image_turns > 0 {
        trim_images_by_age(messages, config.max_image_turns)
    } else {
        messages.to_vec()
    };

    // Second pass: apply per-request image cap before normalization.
    let candidate_messages =
        if count_image_markers_with_latest_tool_results(&age_trimmed, &latest_tool_indices)
            > max_images
        {
            trim_old_images(&age_trimmed, max_images)
        } else {
            age_trimmed
        };

    // Track aggregate decode budget across all normalization in this call.
    let mut remaining_decode_budget = AGGREGATE_DECODE_BUDGET_BYTES;

    // Walk newest-first so the budget is spent on the most recent images and
    // exhaustion sheds the oldest ones. Iterating oldest-first would let stale
    // history consume the budget and drop the image the user just sent, which
    // also contradicts `trim_old_images`. Message order is restored below;
    // `index` remains the original position, so the age/tool-replay decisions
    // keyed on it are unaffected by the traversal direction.
    let mut normalized_messages = Vec::with_capacity(candidate_messages.len());
    for (index, message) in candidate_messages.iter().enumerate().rev() {
        if !should_normalize_message_images(index, message, &latest_tool_indices) {
            normalized_messages.push(replay_message_without_stale_tool_images(
                index,
                message,
                &latest_tool_indices,
            ));
            continue;
        }

        if message.role == "tool"
            && let Some((prepared, _contains_images)) = normalize_native_tool_result_json(
                &message.content,
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
            .await
        {
            normalized_messages.push(ChatMessage {
                role: message.role.clone(),
                content: prepared,
            });
            continue;
        }

        let (cleaned_text, refs) = parse_image_markers(&message.content);
        if refs.is_empty() {
            normalized_messages.push(message.clone());
            continue;
        }

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
        let content = compose_multimodal_content(
            &cleaned_text,
            &normalized.data_uris,
            normalized.skipped_count,
            refs.len(),
        );
        normalized_messages.push(ChatMessage {
            role: message.role.clone(),
            content,
        });
    }

    // Undo the newest-first traversal: callers require original order.
    normalized_messages.reverse();

    // No post-normalization trim: both the age trim and the per-request cap
    // already ran above, before any decode. Normalization emits at most one
    // marker per *successful* reference, so the marker count can only shrink
    // from here — re-running either trim would be a guaranteed no-op.

    Ok(PreparedMessages {
        contains_images: count_image_markers(&normalized_messages) > 0,
        messages: normalized_messages,
    })
}
fn trim_images_by_age(messages: &[ChatMessage], max_turns: usize) -> Vec<ChatMessage> {
    // Count user messages from the end to find the cutoff index.
    let mut user_turn_count = 0usize;
    let mut cutoff = 0usize; // messages at index < cutoff are "too old"
    for (i, m) in messages.iter().enumerate().rev() {
        if m.role == "user" {
            user_turn_count += 1;
            if user_turn_count > max_turns {
                // Everything up to and including this index is too old.
                cutoff = i + 1;
                break;
            }
        }
    }

    if cutoff == 0 {
        return messages.to_vec();
    }

    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i < cutoff && m.role == "user" {
                let (cleaned, refs) = parse_image_markers(&m.content);
                if refs.is_empty() {
                    return m.clone();
                }
                let text = if cleaned.trim().is_empty() {
                    "[image removed from history]".to_string()
                } else {
                    cleaned
                };
                ChatMessage {
                    role: m.role.clone(),
                    content: text,
                }
            } else {
                m.clone()
            }
        })
        .collect()
}

/// Strip image markers from older messages (oldest first) until total image
/// count is within `max_images`. Keeps the text content of each message.
fn trim_old_images(messages: &[ChatMessage], max_images: usize) -> Vec<ChatMessage> {
    let latest_tool_indices = latest_tool_result_indices(messages);
    // Find which messages (by index) contain images, oldest first.
    let image_positions: Vec<(usize, usize)> = messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            should_normalize_message_images(*index, message, &latest_tool_indices)
        })
        .filter_map(|(i, m)| {
            let count = parse_image_markers(&m.content).1.len();
            if count > 0 { Some((i, count)) } else { None }
        })
        .collect();

    // Determine how many images to drop (from the oldest messages).
    let total: usize = image_positions.iter().map(|(_, c)| c).sum();
    let mut to_drop = total.saturating_sub(max_images);

    // Collect indices of messages whose images should be stripped.
    let mut strip_indices = std::collections::HashSet::new();
    for &(idx, count) in &image_positions {
        if to_drop == 0 {
            break;
        }
        strip_indices.insert(idx);
        to_drop = to_drop.saturating_sub(count);
    }

    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if strip_indices.contains(&i) {
                let (cleaned, _) = parse_image_markers(&m.content);
                let text = if cleaned.trim().is_empty() {
                    "[image removed from history]".to_string()
                } else {
                    cleaned
                };
                ChatMessage {
                    role: m.role.clone(),
                    content: text,
                }
            } else {
                replay_message_without_stale_tool_images(i, m, &latest_tool_indices)
            }
        })
        .collect()
}

fn compose_multimodal_message(text: &str, data_uris: &[String]) -> String {
    let mut content = String::new();
    let trimmed = text.trim();

    if !trimmed.is_empty() {
        content.push_str(trimmed);
        content.push_str("\n\n");
    }

    for (index, data_uri) in data_uris.iter().enumerate() {
        if index > 0 {
            content.push('\n');
        }
        content.push_str(IMAGE_MARKER_PREFIX);
        content.push_str(data_uri);
        content.push(']');
    }

    content
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
            Ok(data_uri) => data_uris.push(data_uri),
            Err(error) => {
                skipped_count += 1;
                let error_reason = multimodal_error_reason(&error);
                // Truncate the raw reference so we don't dump a full base64
                // payload into the log, but keep enough to identify the source.
                let marker_preview: String = reference.chars().take(120).collect();
                let error_kind = multimodal_error_kind(&error);
                let attrs = ::serde_json::json!({
                    "message_index": ctx.message_index,
                    "message_role": ctx.role,
                    "source_kind": image_reference_kind(reference),
                    "error_kind": error_kind,
                    "reason": error_reason.as_deref().unwrap_or(""),
                    "marker_preview": marker_preview,
                });
                let is_tool_role = ctx.role == "tool";
                let is_recoverable_load_failure = matches!(
                    error_kind,
                    "image_source_not_found"
                        | "local_read_failed"
                        | "remote_fetch_failed"
                        | "invalid_marker"
                        | "corrupt_image"
                );
                if is_tool_role && is_recoverable_load_failure {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(attrs),
                        "skipping multimodal marker in tool result (likely not a real attachment)"
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

fn compose_multimodal_content(
    text: &str,
    data_uris: &[String],
    skipped_count: usize,
    total_refs: usize,
) -> String {
    if skipped_count == 0 {
        return compose_multimodal_message(text, data_uris);
    }

    let text_with_note = append_skipped_image_note(text, skipped_count, total_refs);
    if data_uris.is_empty() {
        text_with_note.trim().to_string()
    } else {
        compose_multimodal_message(&text_with_note, data_uris)
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
        Some(MultimodalError::InvalidMarker { .. }) => "invalid_marker",
        Some(MultimodalError::RemoteFetchFailed { .. }) => "remote_fetch_failed",
        Some(MultimodalError::LocalReadFailed { .. }) => "local_read_failed",
        Some(MultimodalError::CorruptImage { .. }) => "corrupt_image",
        None => "unknown",
    }
}

fn multimodal_error_reason(error: &anyhow::Error) -> Option<String> {
    match error.downcast_ref::<MultimodalError>() {
        Some(MultimodalError::InvalidMarker { input, reason })
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
    let Some(comma_idx) = source.find(',') else {
        return Err(MultimodalError::InvalidMarker {
            input: source.to_string(),
            reason: "expected data URI payload".to_string(),
        }
        .into());
    };

    let header = &source[..comma_idx];
    let payload = source[comma_idx + 1..].trim();

    if !header.contains(";base64") {
        return Err(MultimodalError::InvalidMarker {
            input: source.to_string(),
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

    validate_mime(source, &mime)?;

    let decoded = STANDARD
        .decode(payload)
        .map_err(|error| MultimodalError::InvalidMarker {
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

    if let Some(cached) = cache.get(source, cache_len, mtime) {
        return Ok(cached.to_string());
    }

    validate_size(
        source,
        usize::try_from(file_len).unwrap_or(usize::MAX),
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
    if ALLOWED_IMAGE_MIME_TYPES.contains(&mime) {
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

/// Admission charge for one decoded canvas, derived from the header alone.
/// Reading dimensions does not decode pixels, so this is cheap enough to run
/// before the budget check — which is the point: an image whose first canvas
/// cannot fit the remaining budget is refused without ever being decoded.
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

    let (width, height) =
        reader
            .into_dimensions()
            .map_err(|error| MultimodalError::CorruptImage {
                input: source.to_string(),
                mime: mime.to_string(),
                reason: error.to_string(),
            })?;

    Ok(u64::from(width) * u64::from(height) * 4)
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

fn validate_animation_frames(
    frames: image::Frames<'_>,
    source: &str,
    mime: &str,
    projected_allocation: u64,
    budget_cap: u64,
) -> Result<u64, ImageValidationFailure> {
    let effective_cap = MAX_DECODED_IMAGE_ALLOC_BYTES.min(budget_cap);
    let aggregate_cap_is_tighter = budget_cap <= MAX_DECODED_IMAGE_ALLOC_BYTES;
    let mut total = 0u64;

    for frame in frames {
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                // Animation decoders normally allocate or reuse a canvas before
                // discovering corrupt frame data. Charge one attempted canvas
                // in addition to the frames that completed, but do not close
                // the entire shared budget unless the decoder identified its
                // allocation limit as the cause.
                let attempted = total.saturating_add(projected_allocation);
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
                total.saturating_add(projected_allocation),
            ));
        }

        let frame_allocation = u64::from(buffer.width()) * u64::from(buffer.height()) * 4;
        let next_total = total.saturating_add(frame_allocation);
        if next_total > effective_cap {
            if aggregate_cap_is_tighter {
                return Err(aggregate_budget_failure(source, mime, next_total));
            }
            return Err(invalid_image_failure(
                source,
                mime,
                format!(
                    "cumulative frame allocation exceeds per-image limit of \
                     {MAX_DECODED_IMAGE_ALLOC_BYTES} bytes"
                ),
                next_total,
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

    Ok(total)
}

/// Validate `bytes` against the aggregate decode budget, then decode.
///
/// The projected allocation is checked *before* the decode so an image that
/// cannot fit is rejected without spending the work. When the projection does
/// not fit, the budget is driven to zero: the caller is mid-way through a
/// sequence of candidates, and leaving a non-zero remainder would let every
/// subsequent candidate re-attempt validation against a budget that can no
/// longer accommodate anything.
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
            *remaining_budget = budget_before_decode.saturating_sub(allocation);
            Ok(allocation)
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

    // Enforce the per-image allocation cap ourselves, before any decoder can
    // allocate. `Limits::max_alloc` is not a reliable guard here: the trait's
    // default `set_limits` only checks `check_support` and `check_dimensions`,
    // and `WebPDecoder` (image 0.25.10) does not override it, so the cap is
    // silently dropped on that path. A 5000x5000 WebP clears the dimension
    // limit and the aggregate budget, yet still projects ~100 MB — above the
    // per-image cap — and would otherwise reach a direct allocation in
    // `DynamicImage::from_decoder` or in the animation frame iterator before
    // the post-decode accounting in `validate_animation_frames` can run.
    //
    // This is the image's own cap, not the shared allowance, so it is an
    // ordinary invalid-image failure: it must not close the budget for
    // unrelated siblings. Nothing has been decoded yet, hence a zero charge.
    if projected_allocation > MAX_DECODED_IMAGE_ALLOC_BYTES {
        return Err(ImageValidationFailure {
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
        });
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
                    Ok(u64::from(image.width()) * u64::from(image.height()) * 4)
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
                    Ok(u64::from(image.width()) * u64::from(image.height()) * 4)
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
                Ok(u64::from(image.width()) * u64::from(image.height()) * 4)
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
        && let Some(mime) = mime_from_extension(ext)
    {
        return Some(mime.to_string());
    }

    mime_from_magic(bytes).map(ToString::to_string)
}

fn normalize_content_type(content_type: &str) -> Option<String> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    if mime.is_empty() { None } else { Some(mime) }
}

fn mime_from_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn mime_from_magic(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        return Some("image/png");
    }

    if bytes.len() >= 3 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }

    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some("image/gif");
    }

    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }

    if bytes.len() >= 2 && bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for mime in ALLOWED_IMAGE_MIME_TYPES {
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

    #[test]
    fn strip_media_markers_replaces_image_local_path() {
        let input = "Look at [IMAGE:/zeroclaw-data/workspace/telegram_files/photo_1.jpg]";
        assert_eq!(strip_media_markers(input), "Look at [media attachment]");
    }

    #[test]
    fn strip_media_markers_replaces_image_data_uri() {
        let input = "Inline [IMAGE:data:image/png;base64,abcd]";
        assert_eq!(strip_media_markers(input), "Inline [media attachment]");
    }

    #[test]
    fn strip_media_markers_replaces_all_supported_kinds() {
        // Mirrors `ATTACHMENT_KINDS` in
        // `crates/zeroclaw-channels/src/util.rs`, which is the source of
        // truth for which marker spellings inbound channels can produce.
        let input = "[IMAGE:/a.jpg] [PHOTO:/b.jpg] [DOCUMENT:/c.pdf] [FILE:/d.zip] [VIDEO:/e.mp4] [VOICE:/f.ogg] [AUDIO:/g.wav]";
        let expected = "[media attachment] [media attachment] [media attachment] [media attachment] [media attachment] [media attachment] [media attachment]";
        assert_eq!(strip_media_markers(input), expected);
    }

    #[test]
    fn strip_media_markers_is_case_insensitive() {
        // Channel parsers uppercase the kind before comparing, so by the time
        // a marker reaches conversation history it is normally upper-case —
        // but accept lower/mixed case too so we don't depend on that
        // invariant downstream.
        let input = "[image:/a.jpg] [Photo:/b.jpg] [video:/c.mp4]";
        let expected = "[media attachment] [media attachment] [media attachment]";
        assert_eq!(strip_media_markers(input), expected);
    }

    #[test]
    fn strip_media_markers_leaves_plain_text_untouched() {
        let input = "No markers here, just text with [brackets] and (parens).";
        assert_eq!(strip_media_markers(input), input);
    }

    #[test]
    fn strip_media_markers_preserves_unrelated_brackets() {
        // Markers that don't match the media kinds are left alone.
        let input = "Use [TODO:foo] and [NOTE:bar] but replace [IMAGE:/x.jpg]";
        assert_eq!(
            strip_media_markers(input),
            "Use [TODO:foo] and [NOTE:bar] but replace [media attachment]"
        );
    }

    // ── loadable audio markers degrade; other media kinds keep their paths ──

    #[test]
    fn strip_unplayable_audio_markers_replaces_loadable_audio_path() {
        let (out, n) = strip_unplayable_audio_markers("hear this [AUDIO:/tmp/clip.wav] now");
        assert_eq!(out, "hear this [media attachment] now");
        assert_eq!(n, 1);
    }

    #[test]
    fn strip_unplayable_audio_markers_degrades_audio_kinds_only() {
        // The delivery contract: DOCUMENT/FILE/VIDEO/PHOTO paths stay
        // model-visible so the agent can hand them to file tools or copy them
        // into outbound reply markers; only the audio kinds degrade.
        let input = "[PHOTO:/a.jpg] [DOCUMENT:/b.pdf] [FILE:/c.zip] [VIDEO:/d.mp4] [VOICE:/e.ogg] [AUDIO:/f.wav]";
        let (out, n) = strip_unplayable_audio_markers(input);
        assert_eq!(
            out,
            "[PHOTO:/a.jpg] [DOCUMENT:/b.pdf] [FILE:/c.zip] [VIDEO:/d.mp4] [media attachment] [media attachment]"
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn audio_marker_kinds_is_subset_of_media_marker_kinds() {
        for kind in AUDIO_MARKER_KINDS {
            assert!(
                MEDIA_MARKER_KINDS.contains(kind),
                "audio kind {kind} missing from the full marker vocabulary"
            );
        }
    }

    #[test]
    fn strip_unplayable_audio_markers_leaves_image_marker_untouched() {
        // `[IMAGE:...]` is handled by `parse_image_markers`; the audio
        // stripper must never touch it (that would drop a resolvable image).
        let (out, n) = strip_unplayable_audio_markers("[IMAGE:/a.png] and [AUDIO:/b.wav]");
        assert_eq!(out, "[IMAGE:/a.png] and [media attachment]");
        assert_eq!(n, 1);
    }

    #[test]
    fn strip_unplayable_audio_markers_preserves_non_loadable_payloads() {
        // Placeholders, prose, a bare filename, and the no-transcription
        // `[Audio: attached]` note are harmless literal text — keep them.
        for input in [
            "[AUDIO:...]",
            "[VOICE:<clip>]",
            "[Audio: attached]",
            "[AUDIO:example.wav]",
        ] {
            let (out, n) = strip_unplayable_audio_markers(input);
            assert_eq!(out, input, "should preserve non-loadable marker: {input}");
            assert_eq!(
                n, 0,
                "non-loadable marker must not count as stripped: {input}"
            );
        }
    }

    #[test]
    fn strip_unplayable_audio_markers_is_case_insensitive() {
        let (out, n) = strip_unplayable_audio_markers("[Audio:/tmp/clip.wav]");
        assert_eq!(out, "[media attachment]");
        assert_eq!(n, 1);
    }

    #[test]
    fn strip_unplayable_audio_markers_handles_data_uri_and_url() {
        let (out, n) = strip_unplayable_audio_markers(
            "[VOICE:data:audio/ogg;base64,AAAA] and [AUDIO:https://x/y.mp3]",
        );
        assert_eq!(out, "[media attachment] and [media attachment]");
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn prepare_messages_strips_tool_result_audio_marker() {
        // The reported failure: a tool result surfaces an audio path. With no
        // images in history, prep must still strip the marker so the raw
        // filesystem path never reaches the provider as literal text.
        let history = vec![
            ChatMessage::user("call the tool and tell me what you hear"),
            ChatMessage::tool("[AUDIO:/tmp/clip.wav] recorded 3:00 PM"),
        ];
        let cfg = MultimodalConfig::default();
        let prepared = prepare_messages_for_provider(&history, &cfg).await.unwrap();
        let tool_msg = prepared
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .expect("tool message survives prep");
        assert!(
            !tool_msg.content.contains("/tmp/clip.wav"),
            "raw audio path must not reach the provider: {}",
            tool_msg.content
        );
        assert!(tool_msg.content.contains("[media attachment]"));
        assert!(!prepared.contains_images);
    }

    #[tokio::test]
    async fn prepare_messages_preserves_document_marker_for_delivery() {
        // A tool result that surfaces a document path must reach the provider
        // intact: the agent copies that path into an outbound reply marker to
        // deliver the file, and file tools read it on request. Only the audio
        // kinds degrade.
        let history = vec![
            ChatMessage::user("send me the report"),
            ChatMessage::tool(
                "[DOCUMENT:/workspace/report.pdf] generated, and [AUDIO:/tmp/note.wav]",
            ),
        ];
        let cfg = MultimodalConfig::default();
        let prepared = prepare_messages_for_provider(&history, &cfg).await.unwrap();
        let tool_msg = prepared
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .expect("tool message survives prep");
        assert!(
            tool_msg
                .content
                .contains("[DOCUMENT:/workspace/report.pdf]"),
            "document path must stay model-visible for delivery: {}",
            tool_msg.content
        );
        assert!(
            !tool_msg.content.contains("/tmp/note.wav"),
            "audio path alongside it must still degrade: {}",
            tool_msg.content
        );
    }

    #[tokio::test]
    async fn prepare_messages_strips_audio_but_keeps_image_marker() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shot.png");
        std::fs::write(&path, valid_png()).unwrap();
        let history = vec![ChatMessage::user(format!(
            "look [IMAGE:{}] and hear [AUDIO:/tmp/clip.wav]",
            path.display()
        ))];
        let cfg = MultimodalConfig::default();
        let prepared = prepare_messages_for_provider(&history, &cfg).await.unwrap();
        let content = &prepared.messages[0].content;
        assert!(
            !content.contains("/tmp/clip.wav"),
            "audio path must be stripped: {content}"
        );
        assert!(content.contains("[media attachment]"));
        // The image marker is still normalized to a data URI alongside it.
        assert!(prepared.contains_images, "image still inlined: {content}");
    }

    #[test]
    fn parse_image_markers_extracts_multiple_markers() {
        let input = "Check this [IMAGE:/tmp/a.png] and this [IMAGE:https://example.com/b.jpg]";
        let (cleaned, refs) = parse_image_markers(input);

        assert_eq!(cleaned, "Check this  and this");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0], "/tmp/a.png");
        assert_eq!(refs[1], "https://example.com/b.jpg");
    }

    #[test]
    fn is_windows_unc_path_accepts_shares_and_rejects_others() {
        assert!(is_windows_unc_path(r"\\server\share\pic.png"));
        assert!(is_windows_unc_path(r"\\server\share\sub\pic.png"));
        // Verbatim / device prefixes are not plain shares.
        assert!(!is_windows_unc_path(r"\\?\C:\Users\me\a.png"));
        assert!(!is_windows_unc_path(r"\\?\UNC\server\share\a.png"));
        assert!(!is_windows_unc_path(r"\\.\PhysicalDrive0"));
        // Needs both a server and a further segment.
        assert!(!is_windows_unc_path(r"\\server"));
        assert!(!is_windows_unc_path(r"\\"));
        // Non-UNC inputs.
        assert!(!is_windows_unc_path("/home/me/a.png"));
        assert!(!is_windows_unc_path(r"C:\Users\me\a.png"));
    }

    #[test]
    fn parse_image_markers_extracts_unc_path() {
        // Regression for theWindows follow-up: `image_info` unwraps the
        // verbatim-UNC prefix (`\\?\UNC\…`) to a plain `\\server\share\…`
        // path, which must be treated as a loadable image reference (not left
        // as literal text) so the image reaches vision models.
        let input = r"File: [IMAGE:\\server\share\pic.png]";
        let (_, refs) = parse_image_markers(input);
        assert_eq!(refs.len(), 1, "UNC marker should be extracted as a ref");
        assert_eq!(refs[0], r"\\server\share\pic.png");
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
        let history = vec![ChatMessage::user(format!(
            "what is in this? [IMAGE:{}]",
            path.display()
        ))];

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
        let history = vec![ChatMessage::user(format!(
            "compare [IMAGE:{}] and [IMAGE:{}]",
            good.display(),
            bad.display()
        ))];

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
        let history = vec![ChatMessage::user(format!(
            "describe [IMAGE:{}]",
            path.display()
        ))];

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
            history.push(ChatMessage::user(format!(
                "old {i} [IMAGE:{}]",
                path.display()
            )));
            history.push(ChatMessage::assistant("ack"));
        }
        let good = temp.path().join("good.png");
        std::fs::write(&good, valid_png()).unwrap();
        history.push(ChatMessage::user(format!(
            "newest [IMAGE:{}]",
            good.display()
        )));

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

    #[tokio::test]
    async fn aggregate_decode_budget_bounds_total_work_per_normalization_call() {
        // Each image is individually under both the size and dimension caps,
        // so only the aggregate budget bounds the total decode cost of one
        // normalization call. Preparation must still succeed and still inline
        // whatever fits inside the budget.
        let temp = tempfile::tempdir().unwrap();
        let mut markers = String::new();
        for i in 0..4 {
            let path = temp.path().join(format!("ok_{i}.png"));
            std::fs::write(&path, valid_png()).unwrap();
            markers.push_str(&format!(" [IMAGE:{}]", path.display()));
        }
        let history = vec![ChatMessage::user(format!("batch{markers}"))];

        let config = MultimodalConfig {
            max_images: 4,
            ..MultimodalConfig::default()
        };
        let prepared = prepare_messages_for_provider(&history, &config)
            .await
            .expect("a batch within budget must prepare cleanly");

        let content = &prepared.messages[0].content;
        assert!(
            prepared.contains_images,
            "valid batch must be inlined: {content}"
        );
        assert!(
            !content.contains("could not be loaded"),
            "nothing in a within-budget batch should be skipped: {content}"
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
            ChatMessage::user(format!("old [IMAGE:{}]", old_path.display())),
            ChatMessage::user(format!("new [IMAGE:{}]", new_path.display())),
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
        let history = vec![ChatMessage::user(format!(
            "compare [IMAGE:{}] with [IMAGE:{}]",
            bad.display(),
            good.display()
        ))];
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
            ChatMessage::user(format!("here is [IMAGE:{}]", old_png.display())),
            ChatMessage::user(format!("check this [IMAGE:{}]", new_gif.display())),
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
        assert_eq!(
            allocation, 8,
            "two 1x1 RGBA frames are charged cumulatively"
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
    async fn gif_admitted_by_projection_but_over_budget_across_frames_is_rejected() {
        // Reproduces the accounting gap the projection alone cannot see: for a
        // two-frame 1x1 GIF, `projected_allocation` (canvas-once) reports 4
        // bytes, but the real cumulative decode is 8. A remaining budget of 6
        // sits between those two numbers, so the header-only pre-check in
        // `validate_within_budget` admits the GIF — the live, budget-aware
        // enforcement inside `validate_image_content` must still catch it
        // rather than silently landing on zero via `saturating_sub`.
        let mut gif = two_frame_gif(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        let mut budget = 6u64;
        validate_within_budget("anim.gif", "image/gif", &gif, &mut budget)
            .await
            .expect_err("an animation whose real cost exceeds the remaining budget must reject");
        assert_eq!(
            budget, 0,
            "a cumulative-budget rejection must close this call's budget, not leave it \
             non-zero for a later candidate to repeat the same bounded overshoot against"
        );
    }

    #[tokio::test]
    async fn gif_over_budget_rejection_stops_a_later_candidate_from_decoding() {
        // The request-level regression the prior review asked for: with two
        // over-budget GIF candidates walked newest-first (mirroring how
        // `prepare_messages_inner` drives `remaining_decode_budget`), the first
        // candidate's cumulative-budget rejection must close the budget so the
        // second candidate is refused by the cheap pre-decode projection check
        // rather than entering `validate_image_content` and repeating the same
        // bounded decode overshoot.
        let mut gif = two_frame_gif(&GIF_VALID_1X1_LZW);
        gif.push(GIF_TRAILER);

        let ((), decodes) = counting_decodes(async {
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
            decodes, 1,
            "only the first candidate may decode; a closed budget must refuse the \
             second via the pre-decode projection check, not by repeating the decode"
        );
    }

    fn two_frame_apng() -> Vec<u8> {
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
        let still = valid_png();
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
            0, 0, 0, 1, // width
            0, 0, 0, 1, // height
            8, 6, 0, 0, 0, // RGBA8, compression, filter, interlace
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
        assert_eq!(allocation, 8, "both APNG canvases are charged");

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
        assert_eq!(allocation, 8, "both WebP canvases are charged");

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

    /// A WebP whose `VP8X` canvas is 5000x5000. That clears the 16,384-pixel
    /// dimension limit and projects ~100 MB of RGBA — above the 64 MiB
    /// per-image cap but below the 256 MiB aggregate budget, which is exactly
    /// the window where `Limits::max_alloc` was the only thing standing
    /// between the payload and a direct decoder allocation.
    ///
    /// `animated` selects the `ANIM`/`ANMF` form; the frames themselves stay
    /// 1x1 so the fixture is small, proving the canvas alone is what gets the
    /// payload refused.
    fn oversized_canvas_webp(animated: bool) -> Vec<u8> {
        // 24-bit little-endian canvas width-1 and height-1.
        const CANVAS_MINUS_ONE: [u8; 3] = [0x87, 0x13, 0x00]; // 4999 => 5000px
        let mut vp8x = vec![if animated { 0x02 } else { 0x00 }, 0, 0, 0];
        vp8x.extend_from_slice(&CANVAS_MINUS_ONE);
        vp8x.extend_from_slice(&CANVAS_MINUS_ONE);

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

    #[tokio::test]
    async fn oversized_webp_canvas_is_refused_before_allocation() {
        // `WebPDecoder` (image 0.25.10) does not override `set_limits`, and the
        // trait default only checks `check_support`/`check_dimensions` — the
        // `max_alloc` cap is silently dropped. Without an explicit pre-decode
        // guard, `DynamicImage::from_decoder` allocates from `total_bytes()`
        // and the animation iterator builds frame buffers, both before any
        // post-decode accounting can run. Every WebP shape must be refused on
        // the canvas projection alone.
        for animated in [false, true] {
            let webp = oversized_canvas_webp(animated);

            let error = validate_image_content(
                "big.webp",
                "image/webp",
                &webp,
                AGGREGATE_DECODE_BUDGET_BYTES,
            )
            .await
            .expect_err("a canvas above the per-image cap must be refused, not allocated");

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
    async fn oversized_webp_canvas_does_not_close_the_shared_budget() {
        // The per-image cap is this image's own ceiling, not the shared
        // allowance. Exceeding it is an ordinary invalid-image failure, so an
        // unrelated sibling must keep its budget — the same contract the GIF
        // sibling regressions above assert.
        let webp = oversized_canvas_webp(false);
        let mut budget = AGGREGATE_DECODE_BUDGET_BYTES;

        validate_within_budget("big.webp", "image/webp", &webp, &mut budget)
            .await
            .expect_err("an over-cap canvas must be refused");

        assert_eq!(
            budget, AGGREGATE_DECODE_BUDGET_BYTES,
            "nothing was decoded, so the shared budget must be untouched for siblings"
        );

        let allocation = validate_within_budget("ok.png", "image/png", &valid_png(), &mut budget)
            .await
            .expect("an unrelated valid sibling must still be admitted");
        assert_eq!(
            budget,
            AGGREGATE_DECODE_BUDGET_BYTES - allocation,
            "only the sibling's real decode is charged"
        );
    }

    #[tokio::test]
    async fn corrupt_data_uri_is_skipped() {
        let b64 = STANDARD.encode([0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
        let history = vec![ChatMessage::user(format!(
            "look [IMAGE:data:image/png;base64,{b64}]"
        ))];

        let prepared = prepare_messages_for_provider(&history, &MultimodalConfig::default())
            .await
            .expect("corrupt data URI must not fail preparation");

        assert!(!prepared.contains_images);
        assert!(prepared.messages[0].content.contains("could not be loaded"));
    }

    #[test]
    fn parse_image_markers_collapses_line_wrapped_path() {
        // Terminal-wrapped paste: a long path split across two rows with
        // leading indentation should be recovered into the original path.
        let input = "from the logs whether the agent emits\n  [IMAGE:/home/zeroclaw_user/.zeroclaw/workspace/signal_i\n  nbound/attachment.jpg] (which the\n  channel resolves)";
        let (_, refs) = parse_image_markers(input);
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0],
            "/home/zeroclaw_user/.zeroclaw/workspace/signal_inbound/attachment.jpg"
        );
    }

    #[test]
    fn parse_image_markers_leaves_placeholder_markers_as_literal_text() {
        // Illustrative markdown like `[IMAGE:...]` or `[IMAGE:<path>]`
        // (e.g. in agent-authored prose the user quotes back) is not a
        // loadable reference and must stay as literal text — otherwise the
        // multimodal loader errors every turn the conversation replays.
        let input = "example: `[IMAGE:...]` or `[IMAGE:<path>]` or `[IMAGE:example.png]`";
        let (cleaned, refs) = parse_image_markers(input);
        assert!(
            refs.is_empty(),
            "no placeholder should be treated as a loadable ref, got: {refs:?}"
        );
        assert!(cleaned.contains("[IMAGE:...]"));
        assert!(cleaned.contains("[IMAGE:<path>]"));
        assert!(cleaned.contains("[IMAGE:example.png]"));
    }

    #[test]
    fn parse_image_markers_preserves_spaces_in_path() {
        // Spaces within a single-line marker are legitimate (paths can
        // contain spaces) and must survive unchanged.
        let input = "look at [IMAGE:/tmp/my photos/beetle.png] please";
        let (_, refs) = parse_image_markers(input);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0], "/tmp/my photos/beetle.png");
    }

    #[test]
    fn parse_image_markers_keeps_invalid_empty_marker() {
        let input = "hello [IMAGE:] world";
        let (cleaned, refs) = parse_image_markers(input);

        assert_eq!(cleaned, "hello [IMAGE:] world");
        assert!(refs.is_empty());
    }

    #[tokio::test]
    async fn prepare_messages_normalizes_local_image_to_data_uri() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("sample.png");

        // Minimal PNG signature bytes are enough for MIME detection.
        std::fs::write(&image_path, valid_png()).unwrap();

        let messages = vec![ChatMessage::user(format!(
            "Please inspect this screenshot [IMAGE:{}]",
            image_path.display()
        ))];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .unwrap();

        assert!(prepared.contains_images);
        assert_eq!(prepared.messages.len(), 1);

        let (cleaned, refs) = parse_image_markers(&prepared.messages[0].content);
        assert_eq!(cleaned, "Please inspect this screenshot");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].starts_with("data:image/png;base64,"));
    }

    #[tokio::test]
    async fn prepare_messages_normalizes_tool_message_local_image_to_data_uri() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("tool-sample.png");

        std::fs::write(&image_path, valid_png()).unwrap();

        let messages = vec![ChatMessage::tool(format!(
            "<tool_result name=\"image_gen\">\nGenerated image [IMAGE:{}]\n</tool_result>",
            image_path.display()
        ))];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .unwrap();

        assert!(prepared.contains_images);
        assert_eq!(prepared.messages.len(), 1);
        assert_eq!(prepared.messages[0].role, "tool");

        let (cleaned, refs) = parse_image_markers(&prepared.messages[0].content);
        assert!(cleaned.contains("<tool_result name=\"image_gen\">"));
        assert!(cleaned.contains("Generated image"));
        assert_eq!(refs.len(), 1);
        assert!(refs[0].starts_with("data:image/png;base64,"));
    }

    #[tokio::test]
    async fn prepare_messages_preserves_native_tool_result_json_shape() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("native-tool-result.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let native_tool_content = serde_json::json!({
            "tool_call_id": "tc1",
            "content": format!("see attached [IMAGE:{}]", image_path.display().to_string()),
        })
        .to_string();

        let messages = vec![ChatMessage::tool(native_tool_content)];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("preparation should succeed for native tool-result JSON");

        assert!(prepared.contains_images);
        assert_eq!(prepared.messages.len(), 1);
        assert_eq!(prepared.messages[0].role, "tool");

        let value: serde_json::Value = serde_json::from_str(&prepared.messages[0].content)
            .expect("prepared tool message must remain valid JSON");

        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc1"),
            "tool_call_id must survive multimodal preprocessing unchanged"
        );

        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content must remain a JSON string");
        assert!(
            inner.contains("see attached"),
            "surrounding text in tool content should survive normalization"
        );
        assert!(
            inner.contains("data:image/png;base64,"),
            "local image path inside tool content should be rewritten to a data URI"
        );
        assert!(
            !inner.contains("native-tool-result.png"),
            "raw local path must not leak after normalization"
        );
    }

    #[tokio::test]
    async fn prepare_messages_preserves_native_tool_json_when_image_is_skipped() {
        let native_tool_content = serde_json::json!({
            "tool_call_id": "tc1",
            "content": "generated screenshot [IMAGE:https://example.com/missing.png]",
        })
        .to_string();

        let prepared = prepare_messages_for_provider(
            &[ChatMessage::tool(native_tool_content)],
            &MultimodalConfig::default(),
        )
        .await
        .expect("skipped native tool image should not fail message preparation");

        assert!(!prepared.contains_images);
        assert_eq!(prepared.messages.len(), 1);

        let value: serde_json::Value = serde_json::from_str(&prepared.messages[0].content)
            .expect("native tool result must remain valid JSON");
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc1")
        );

        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert!(inner.contains("generated screenshot"));
        assert!(inner.contains("1 attached image(s) could not be loaded"));
        assert!(!inner.contains("[IMAGE:"));
        assert!(!inner.contains("https://example.com/missing.png"));
    }

    #[tokio::test]
    async fn prepare_messages_preserves_native_tool_json_with_mixed_images() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("mixed-native-tool-result.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let native_tool_content = serde_json::json!({
            "tool_call_id": "tc1",
            "content": format!(
                "generated [IMAGE:{}] and [IMAGE:https://example.com/missing.png]",
                image_path.display()
            ),
        })
        .to_string();

        let prepared = prepare_messages_for_provider(
            &[ChatMessage::tool(native_tool_content)],
            &MultimodalConfig::default(),
        )
        .await
        .expect("valid native tool image should survive while bad ref is skipped");

        assert!(prepared.contains_images);
        assert_eq!(prepared.messages.len(), 1);

        let value: serde_json::Value = serde_json::from_str(&prepared.messages[0].content)
            .expect("native tool result must remain valid JSON");
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc1")
        );

        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert!(inner.contains("generated"));
        assert!(inner.contains("data:image/png;base64,"));
        assert!(inner.contains("1 of 2 attached image(s) could not be loaded"));
        assert!(!inner.contains("mixed-native-tool-result.png"));
        assert!(!inner.contains("https://example.com/missing.png"));
    }

    #[tokio::test]
    async fn prepare_messages_strips_stale_native_tool_result_images() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("stale-native-tool-result.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let native_tool_content = serde_json::json!({
            "tool_call_id": "tc1",
            "content": format!("generated screenshot [IMAGE:{}]", image_path.display().to_string()),
        })
        .to_string();

        let messages = vec![
            ChatMessage::tool(native_tool_content),
            ChatMessage {
                role: "assistant".to_string(),
                content: "I generated the screenshot.".to_string(),
            },
            ChatMessage::user("What happened next?".to_string()),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("preparation should strip stale tool images without loading them");

        assert!(
            !prepared.contains_images,
            "stale tool-result images should not keep the request in vision mode"
        );

        let value: serde_json::Value = serde_json::from_str(&prepared.messages[0].content)
            .expect("stale native tool result should remain valid JSON");
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc1")
        );

        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert!(inner.contains("generated screenshot"));
        assert!(!inner.contains("[IMAGE:"));
        assert!(!inner.contains("data:image"));
        assert!(!inner.contains("stale-native-tool-result.png"));
    }

    #[tokio::test]
    async fn prepare_messages_strips_stale_prompt_tool_result_images() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("stale-prompt-tool-result.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let messages = vec![
            ChatMessage::user(format!(
                "[Tool results]\n<tool_result name=\"image_gen\">Generated [IMAGE:{}]</tool_result>",
                image_path.display()
            )),
            ChatMessage {
                role: "assistant".to_string(),
                content: "I generated the screenshot.".to_string(),
            },
            ChatMessage::user("Continue.".to_string()),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("preparation should strip stale prompt-mode tool images");

        assert!(!prepared.contains_images);
        assert!(prepared.messages[0].content.contains("[Tool results]"));
        assert!(prepared.messages[0].content.contains("Generated"));
        assert!(!prepared.messages[0].content.contains("[IMAGE:"));
        assert!(!prepared.messages[0].content.contains("data:image"));
        assert!(
            !prepared.messages[0]
                .content
                .contains("stale-prompt-tool-result.png")
        );
    }

    #[tokio::test]
    async fn prepare_messages_strips_stale_tool_image_while_normalizing_current_user_image() {
        let temp = tempfile::tempdir().unwrap();
        let stale_path = temp.path().join("stale-tool-result.png");
        let fresh_path = temp.path().join("fresh-user-image.png");
        let png = valid_png();
        std::fs::write(&stale_path, &png).unwrap();
        std::fs::write(&fresh_path, &png).unwrap();

        let native_tool_content = serde_json::json!({
            "tool_call_id": "tc1",
            "content": format!("generated screenshot [IMAGE:{}]", stale_path.display().to_string()),
        })
        .to_string();

        let messages = vec![
            ChatMessage::tool(native_tool_content),
            ChatMessage {
                role: "assistant".to_string(),
                content: "I generated the screenshot.".to_string(),
            },
            ChatMessage::user(format!(
                "Now inspect this [IMAGE:{}]",
                fresh_path.display().to_string()
            )),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("preparation should strip stale tool images and normalize current user image");

        assert!(prepared.contains_images);

        let value: serde_json::Value = serde_json::from_str(&prepared.messages[0].content)
            .expect("stale native tool result should remain valid JSON");
        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert!(inner.contains("generated screenshot"));
        assert!(!inner.contains("[IMAGE:"));
        assert!(!inner.contains("data:image"));
        assert!(!inner.contains("stale-tool-result.png"));

        let (cleaned, refs) = parse_image_markers(&prepared.messages[2].content);
        assert_eq!(cleaned, "Now inspect this");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].starts_with("data:image/png;base64,"));
        assert!(
            !prepared.messages[2]
                .content
                .contains("fresh-user-image.png")
        );
    }

    #[test]
    fn count_image_markers_ignores_stale_tool_results() {
        let messages = vec![
            ChatMessage::tool("[IMAGE:/tmp/stale-tool.png]\nGenerated".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "Done.".to_string(),
            },
            ChatMessage::user("Next question".to_string()),
        ];

        assert_eq!(count_image_markers(&messages), 0);

        let messages = vec![
            ChatMessage::user("Create an image".to_string()),
            ChatMessage::tool("[IMAGE:/tmp/latest-tool.png]\nGenerated".to_string()),
        ];

        assert_eq!(count_image_markers(&messages), 1);
    }

    #[test]
    fn count_latest_user_image_markers_scopes_to_newest_user_message() {
        // No user messages at all -> zero.
        assert_eq!(count_latest_user_image_markers(&[]), 0);

        // The newest user message carries the image -> counted (the user just
        // sent it; the vision router surfaces a capability error).
        let just_sent = vec![
            ChatMessage::user("hi".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "hello".to_string(),
            },
            ChatMessage::user("look at this [IMAGE:/tmp/a.png]".to_string()),
        ];
        assert_eq!(count_latest_user_image_markers(&just_sent), 1);

        // An earlier user message carried an image, but the newest user message
        // is plain text -> zero. This is the poison-prevention case: the carried
        // over marker must NOT keep re-triggering the capability error.
        let carried_over = vec![
            ChatMessage::user("look at this [IMAGE:/tmp/a.png]".to_string()),
            ChatMessage::user("what is WAL?".to_string()),
        ];
        assert_eq!(count_latest_user_image_markers(&carried_over), 0);
        // The history-wide count still sees the carried-over marker, which is
        // why the router must distinguish the two.
        assert_eq!(count_user_image_markers(&carried_over), 1);

        // A trailing tool-result carrier does not mask the real latest user
        // message (its markers are not user-sent and must not be counted here).
        let trailing_tool_result = vec![
            ChatMessage::user("inspect [IMAGE:/tmp/a.png]".to_string()),
            ChatMessage::tool("[IMAGE:/tmp/tool.png]\nGenerated".to_string()),
        ];
        assert_eq!(count_latest_user_image_markers(&trailing_tool_result), 1);
    }

    #[tokio::test]
    async fn prepare_messages_trims_excess_images_from_older_messages() {
        // 3 messages, each with 1 image — max is 2.
        // The oldest message's image should be stripped.
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/old.png]\nOld caption".to_string()),
            ChatMessage::user("[IMAGE:/tmp/mid.png]\nMid caption".to_string()),
            ChatMessage::user("[IMAGE:/tmp/new.png]\nNew caption".to_string()),
        ];

        // Should not error — instead trims oldest. (Will error on
        // normalize_image_reference for the surviving images since
        // /tmp/mid.png and /tmp/new.png don't exist, but the trimming
        // itself should succeed.)
        let trimmed = trim_old_images(&messages, 2);
        assert_eq!(trimmed.len(), 3);

        // Oldest message should have image stripped
        let (_, refs0) = parse_image_markers(&trimmed[0].content);
        assert!(refs0.is_empty(), "oldest image should be stripped");
        assert!(trimmed[0].content.contains("Old caption"));

        // Newer messages keep their images
        let (_, refs1) = parse_image_markers(&trimmed[1].content);
        assert_eq!(refs1.len(), 1);
        let (_, refs2) = parse_image_markers(&trimmed[2].content);
        assert_eq!(refs2.len(), 1);
    }

    #[test]
    fn trim_old_images_replaces_image_only_message() {
        // A message with only an image and no text should get a placeholder.
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/old.png]".to_string()),
            ChatMessage::user("[IMAGE:/tmp/new.png]\nKeep this".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 1);
        assert_eq!(trimmed[0].content, "[image removed from history]");
        assert!(trimmed[1].content.contains("[IMAGE:/tmp/new.png]"));
    }

    #[test]
    fn trim_old_images_multi_image_message_stripped_as_unit() {
        // A single message has 3 images. We need to drop 2 to reach max=1.
        // But trimming works at message granularity — the entire message gets
        // stripped (all 3 images removed), which over-trims to 0. The newest
        // message (text-only) is untouched.
        let messages = vec![
            ChatMessage::user(
                "[IMAGE:/tmp/a.png]\n[IMAGE:/tmp/b.png]\n[IMAGE:/tmp/c.png]\nThree pics"
                    .to_string(),
            ),
            ChatMessage::user("Just text, no images".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 1);
        assert_eq!(trimmed.len(), 2);
        // All images in the first message are gone, but text remains
        let (_, refs0) = parse_image_markers(&trimmed[0].content);
        assert!(refs0.is_empty());
        assert!(trimmed[0].content.contains("Three pics"));
        // Second message unchanged
        assert_eq!(trimmed[1].content, "Just text, no images");
    }

    #[test]
    fn trim_old_images_skips_assistant_messages() {
        // Assistant messages with image markers should not be counted or stripped.
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: "[IMAGE:/tmp/assistant.png]\nAssistant generated".to_string(),
            },
            ChatMessage::user("[IMAGE:/tmp/user1.png]\nFirst".to_string()),
            ChatMessage::user("[IMAGE:/tmp/user2.png]\nSecond".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 1);
        // Assistant message untouched (not counted toward limit)
        assert!(trimmed[0].content.contains("[IMAGE:/tmp/assistant.png]"));
        // Oldest user image stripped
        let (_, refs1) = parse_image_markers(&trimmed[1].content);
        assert!(refs1.is_empty());
        assert!(trimmed[1].content.contains("First"));
        // Newest user image kept
        let (_, refs2) = parse_image_markers(&trimmed[2].content);
        assert_eq!(refs2.len(), 1);
    }

    #[test]
    fn trim_old_images_counts_latest_tool_messages() {
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/user-old.png]\nOldest".to_string()),
            ChatMessage::tool("[IMAGE:/tmp/tool-new.png]\nGenerated".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 1);
        let (_, refs0) = parse_image_markers(&trimmed[0].content);
        assert!(refs0.is_empty(), "oldest user image should be stripped");
        assert!(trimmed[0].content.contains("Oldest"));

        let (_, refs1) = parse_image_markers(&trimmed[1].content);
        assert_eq!(refs1.len(), 1);
    }

    #[test]
    fn trim_old_images_no_trimming_when_under_limit() {
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/a.png]\nCaption A".to_string()),
            ChatMessage::user("[IMAGE:/tmp/b.png]\nCaption B".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 5);
        // Nothing should change — both images are under the limit
        assert_eq!(trimmed[0].content, messages[0].content);
        assert_eq!(trimmed[1].content, messages[1].content);
    }

    #[test]
    fn trim_old_images_no_trimming_when_exactly_at_limit() {
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/a.png]\nA".to_string()),
            ChatMessage::user("[IMAGE:/tmp/b.png]\nB".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 2);
        assert_eq!(trimmed[0].content, messages[0].content);
        assert_eq!(trimmed[1].content, messages[1].content);
    }

    #[test]
    fn trim_old_images_empty_messages() {
        let trimmed = trim_old_images(&[], 4);
        assert!(trimmed.is_empty());
    }

    #[test]
    fn trim_old_images_interleaved_roles() {
        // Realistic conversation: user sends image, assistant replies, user sends
        // another image, etc. Only user messages should be candidates for trimming.
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/1.png]\nLook at this".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "I see a photo.".to_string(),
            },
            ChatMessage::user("[IMAGE:/tmp/2.png]\nWhat about this?".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "That's a chart.".to_string(),
            },
            ChatMessage::user("[IMAGE:/tmp/3.png]\nAnd this one".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 2);
        assert_eq!(trimmed.len(), 5);
        // Oldest user image stripped
        let (_, refs0) = parse_image_markers(&trimmed[0].content);
        assert!(refs0.is_empty());
        assert!(trimmed[0].content.contains("Look at this"));
        // Assistant messages untouched
        assert_eq!(trimmed[1].content, "I see a photo.");
        assert_eq!(trimmed[3].content, "That's a chart.");
        // Two newest user images kept
        let (_, refs2) = parse_image_markers(&trimmed[2].content);
        assert_eq!(refs2.len(), 1);
        let (_, refs4) = parse_image_markers(&trimmed[4].content);
        assert_eq!(refs4.len(), 1);
    }

    #[test]
    fn trim_old_images_strips_multiple_oldest_messages() {
        // 5 user images, max 1 — should strip the first 4 messages' images.
        let messages: Vec<ChatMessage> = (1..=5)
            .map(|i| ChatMessage::user(format!("[IMAGE:/tmp/{i}.png]\nCaption {i}")))
            .collect();

        let trimmed = trim_old_images(&messages, 1);
        assert_eq!(trimmed.len(), 5);
        for (i, msg) in trimmed.iter().enumerate().take(4) {
            let (_, refs) = parse_image_markers(&msg.content);
            assert!(refs.is_empty(), "message {i} should have images stripped");
            assert!(msg.content.contains(&format!("Caption {}", i + 1)));
        }
        // Only the last message keeps its image
        let (_, refs_last) = parse_image_markers(&trimmed[4].content);
        assert_eq!(refs_last.len(), 1);
    }

    #[tokio::test]
    async fn prepare_messages_trims_then_normalizes_surviving_images() {
        // End-to-end: 3 images, max 2. After trimming the oldest, the two
        // surviving images should be normalized (base64-encoded) successfully.
        let temp = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for name in ["old.png", "mid.png", "new.png"] {
            let p = temp.path().join(name);
            std::fs::write(&p, valid_png()).unwrap();
            paths.push(p);
        }

        let messages = vec![
            ChatMessage::user(format!("[IMAGE:{}]\nOld", paths[0].display().to_string())),
            ChatMessage::user(format!("[IMAGE:{}]\nMid", paths[1].display().to_string())),
            ChatMessage::user(format!("[IMAGE:{}]\nNew", paths[2].display().to_string())),
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
        assert!(!result.messages[0].content.contains("data:image"));
        assert!(result.messages[0].content.contains("Old"));
        // Second and third should have base64-encoded images
        assert!(result.messages[1].content.contains("data:image"));
        assert!(result.messages[2].content.contains("data:image"));
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
            messages.push(ChatMessage::user(format!(
                "[IMAGE:{}]\nImage {i}",
                p.display()
            )));
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

        // Output is capped to exactly max_images...
        let surviving = result
            .messages
            .iter()
            .filter(|m| m.content.contains("data:image"))
            .count();
        assert_eq!(surviving, 4, "output should keep exactly max_images");

        // ...and it is the newest four that survive; the oldest five are stripped.
        for (i, m) in result.messages.iter().enumerate() {
            if i < 5 {
                assert!(
                    !m.content.contains("data:image"),
                    "oldest message {i} should be capped out"
                );
                assert!(m.content.contains(&format!("Image {i}")));
            } else {
                assert!(
                    m.content.contains("data:image"),
                    "newest message {i} should survive the cap"
                );
            }
        }
    }

    #[tokio::test]
    async fn prepare_messages_skips_remote_url_when_disabled() {
        let messages = vec![ChatMessage::user(
            "Look [IMAGE:https://example.com/img.png]".to_string(),
        )];

        let result = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("disabled remote image should be skipped");

        assert!(!result.contains_images);
        assert_eq!(result.messages.len(), 1);
        assert!(result.messages[0].content.contains("Look"));
        assert!(
            result.messages[0]
                .content
                .contains("1 attached image(s) could not be loaded")
        );
        assert!(
            !result.messages[0]
                .content
                .contains("https://example.com/img.png")
        );
    }

    #[tokio::test]
    async fn prepare_messages_skips_oversized_local_image() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("big.png");

        let bytes = vec![0u8; 1024 * 1024 + 1];
        std::fs::write(&image_path, bytes).unwrap();

        let messages = vec![ChatMessage::user(format!(
            "[IMAGE:{}]",
            image_path.display()
        ))];
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

        let messages = vec![ChatMessage::user(format!(
            "Look [IMAGE:{}] and [IMAGE:https://example.com/missing.png]",
            image_path.display()
        ))];

        let result = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("valid local image should survive while remote image is skipped");

        assert!(result.contains_images);
        assert!(
            result.messages[0]
                .content
                .contains("data:image/png;base64,")
        );
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

    #[tokio::test]
    async fn failed_images_consume_budget_under_pre_normalization_trim() {
        // BEHAVIOR CHANGE: the per-request image cap is now applied
        // *before* normalization, so a newer image that fails to load does
        // consume budget and can evict an older valid one.
        //
        // Rationale: full pixel validation is CPU/memory intensive, unlike the
        // prior header-only sniff. Normalizing every candidate in a long
        // history before capping created an unbounded resource sink — a
        // history of N images forced N full decodes even though only
        // `max_images` are ever sent. Bounding decode work takes precedence
        // over preserving an older image when a newer reference is broken.
        //
        // The aggregate decode budget (`AGGREGATE_DECODE_BUDGET_BYTES`)
        // provides the second bound for images that survive the cap.
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("older-valid.png");
        std::fs::write(&image_path, valid_png()).unwrap();

        let messages = vec![
            ChatMessage::user(format!(
                "Older valid image [IMAGE:{}]",
                image_path.display()
            )),
            ChatMessage::user(
                "Newer broken image [IMAGE:https://example.com/missing.png]".to_string(),
            ),
        ];
        let config = MultimodalConfig {
            max_images: 1,
            max_image_size_mb: 5,
            allow_remote_fetch: false,
            ..Default::default()
        };

        let result = prepare_messages_for_provider(&messages, &config)
            .await
            .expect("a broken newer image must not fail the whole turn");

        // The cap kept only the newest marker, which then failed to load, so
        // no image survives — but the turn still succeeds and the model is
        // told what happened.
        assert!(
            !result.contains_images,
            "newest marker failed to load, so no image should be inlined: {:?}",
            result.messages
        );
        assert!(
            result.messages[0].content.contains("Older valid image"),
            "older message text must survive the cap"
        );
        assert!(
            !result.messages[0]
                .content
                .contains("data:image/png;base64,"),
            "older image was capped out before normalization"
        );
        assert!(result.messages[1].content.contains("Newer broken image"));
        assert!(
            result.messages[1]
                .content
                .contains("1 attached image(s) could not be loaded")
        );
        assert!(
            !result.messages[1]
                .content
                .contains("https://example.com/missing.png"),
            "raw URL must not leak to the model"
        );
    }

    #[test]
    fn extract_ollama_image_payload_supports_data_uris() {
        let payload = extract_ollama_image_payload("data:image/png;base64,abcd==")
            .expect("payload should be extracted");
        assert_eq!(payload, "abcd==");
    }

    #[test]
    fn parse_image_markers_strips_markers_leaving_caption() {
        let input = "[IMAGE:/tmp/photo.jpg]\n\nDescribe this screenshot";
        let (cleaned, refs) = parse_image_markers(input);
        assert_eq!(cleaned, "Describe this screenshot");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0], "/tmp/photo.jpg");
    }

    #[test]
    fn parse_image_markers_image_only_message_becomes_empty() {
        let input = "[IMAGE:/tmp/photo.jpg]";
        let (cleaned, refs) = parse_image_markers(input);
        assert!(
            cleaned.is_empty(),
            "expected empty string, got: {cleaned:?}"
        );
        assert_eq!(refs.len(), 1);
    }
}
