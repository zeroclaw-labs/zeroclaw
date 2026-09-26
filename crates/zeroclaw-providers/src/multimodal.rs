use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Client;
use sha2::{Digest as _, Sha256};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use zeroclaw_api::media::{
    PROVIDER_IMAGE_MIME_TYPES, image_mime_from_extension, image_mime_from_magic,
    is_provider_image_mime,
};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_config::schema::{MultimodalConfig, build_runtime_proxy_client_with_timeouts};

pub const IMAGE_MARKER_PREFIX: &str = "[IMAGE:";

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

fn collapse_wrapped_marker(raw: &str) -> Cow<'_, str> {
    if !raw.contains('\n') && !raw.contains('\r') {
        return Cow::Borrowed(raw.trim());
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
    Cow::Owned(out.trim().to_string())
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

/// Walk `content` once, reporting every span that survives as text through
/// `on_text` and every loadable, collapsed image reference through `on_ref`.
/// Both [`parse_image_markers`] and [`image_marker_summary`] are built on
/// this scanner so the marker grammar has one definition.
fn scan_image_markers(content: &str, mut on_text: impl FnMut(&str), mut on_ref: impl FnMut(&str)) {
    let mut cursor = 0usize;

    while let Some(rel_start) = content[cursor..].find(IMAGE_MARKER_PREFIX) {
        let start = cursor + rel_start;
        on_text(&content[cursor..start]);

        let marker_start = start + IMAGE_MARKER_PREFIX.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            on_text(&content[start..]);
            return;
        };

        let end = marker_start + rel_end;
        let candidate = collapse_wrapped_marker(&content[marker_start..end]);

        if candidate.is_empty() || !is_loadable_image_reference(&candidate) {
            // Preserve the original marker text (placeholders like
            // `[IMAGE:...]` or `[IMAGE:<path>]` should survive as prose
            // rather than triggering a loader error).
            on_text(&content[start..=end]);
        } else {
            on_ref(candidate.as_ref());
        }

        cursor = end + 1;
    }

    if cursor < content.len() {
        on_text(&content[cursor..]);
    }
}

pub fn parse_image_markers(content: &str) -> (String, Vec<String>) {
    let mut cleaned = String::with_capacity(content.len());
    let mut refs = Vec::new();
    scan_image_markers(
        content,
        |text| cleaned.push_str(text),
        |reference| refs.push(reference.to_string()),
    );
    (cleaned.trim().to_string(), refs)
}

/// Byte count of the non-marker text and the number of loadable references,
/// computed by the same scanner as `parse_image_markers` without building
/// the cleaned string or copying references.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageMarkerSummary {
    pub text_bytes: usize,
    pub image_refs: usize,
}

pub fn image_marker_summary(content: &str) -> ImageMarkerSummary {
    let mut summary = ImageMarkerSummary::default();
    scan_image_markers(
        content,
        |text| summary.text_bytes += text.len(),
        |_| summary.image_refs += 1,
    );
    summary
}

pub fn count_image_markers(messages: &[ChatMessage]) -> usize {
    let current_turn_tool_indices = current_turn_tool_result_indices(messages);
    count_image_markers_with_current_turn_tool_results(messages, &current_turn_tool_indices)
}

fn count_image_markers_with_current_turn_tool_results(
    messages: &[ChatMessage],
    current_turn_tool_result_indices: &HashSet<usize>,
) -> usize {
    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            should_normalize_message_images(*index, message, current_turn_tool_result_indices)
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

/// Force-compile this module's lazy regexes on the caller's thread.
///
/// A cold `regex` compile descends through dozens of `regex_automata` NFA
/// compiler frames; `strip_media_markers` and the audio-marker checks run
/// deep inside turn processing, so the registry builder warms both here —
/// on its own dedicated thread — before any turn stack exists.
pub fn warm_lazy_regexes() {
    // Force the fn-local media-marker regex by running one strip; the
    // result is unused, only the initialization matters.
    let _ = strip_media_markers("");
    std::sync::LazyLock::force(&AUDIO_MARKER_RE);
}

/// Text a degraded media marker is replaced with before the history reaches
/// a model that cannot consume the payload. The model may echo it verbatim
/// into a reply, so it is plain prose rather than bracket syntax: it must
/// never parse as an outbound delivery marker (`[KIND:target]`, which the
/// channel parsers key on `[` and `:`), and it contains no JSON-special
/// characters so a marker replaced inside a native tool-result blob leaves
/// the surrounding object valid.
pub const MEDIA_PLACEHOLDER: &str = "(media attachment omitted)";

pub fn strip_media_markers(text: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(
            r"(?i)\[(?:{}):[^\]]*\]",
            MEDIA_MARKER_KINDS.join("|")
        ))
        .expect("static media-marker regex must compile")
    });
    RE.replace_all(text, MEDIA_PLACEHOLDER).into_owned()
}

/// Matches the audio-kind markers ([`AUDIO_MARKER_KINDS`]), capturing the
/// payload for the loadable-reference check.
static AUDIO_MARKER_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"(?i)\[(?:{}):([^\]]*)\]",
        AUDIO_MARKER_KINDS.join("|")
    ))
    .expect("static audio-marker regex must compile")
});

/// Replace audio markers (`[AUDIO:...]`, `[VOICE:...]`) whose payload is a
/// *loadable* reference (absolute path, `http(s)://` URL, or `data:` URI) with
/// the same [`MEDIA_PLACEHOLDER`] the degrade path uses, returning the
/// rewritten text and the number of markers replaced.
///
/// Non-loadable payloads are left as literal text — placeholders (`[AUDIO:...]`),
/// prose (`[AUDIO:<clip>]`), and the no-transcription note (`[Audio: attached]`)
/// are harmless and must survive — mirroring how [`parse_image_markers`]
/// preserves non-loadable `[IMAGE:...]` markers. Runs over the raw string so it
/// also cleans a marker embedded in a native tool-result JSON blob
/// (`{"content":"…[AUDIO:/clip.wav]…"}`); see [`MEDIA_PLACEHOLDER`] for why
/// the surrounding object stays valid.
fn strip_unplayable_audio_markers(text: &str) -> (String, usize) {
    let mut stripped = 0usize;
    let out = AUDIO_MARKER_RE.replace_all(text, |caps: &regex::Captures<'_>| {
        let payload = collapse_wrapped_marker(&caps[1]);
        if !payload.is_empty() && is_loadable_image_reference(&payload) {
            stripped += 1;
            MEDIA_PLACEHOLDER.to_string()
        } else {
            // Preserve placeholder/prose markers verbatim.
            caps[0].to_string()
        }
    });
    (out.into_owned(), stripped)
}

/// Apply `rewrite` to the model-visible text of one history message.
///
/// An assistant message in native tool-call history is the JSON envelope
/// built by the runtime (`{"content": …, "tool_calls": […],
/// "reasoning_content"?: …}`) and parsed back by the provider adapters.
/// It counts as an envelope only when its `tool_calls` value deserializes
/// as `Vec<ToolCall>` — the same predicate every provider adapter uses to
/// decide that an assistant message is a tool-call envelope — so a blob
/// the adapters would replay as plain text (absent, null, non-array, or
/// malformed call list) is sanitized as plain text, failing closed to the
/// whole-string rewrite instead of being protected field-wise.
///
/// Only its `content` string is rewritten: `reasoning_content` carries
/// signed thinking blocks that must replay byte-for-byte, and
/// `tool_calls[].extra_content` carries provider signatures. The envelope
/// is re-serialized only when `content` actually changed, so an untouched
/// envelope stays byte-identical. Every other message is rewritten as a
/// whole string, which keeps a marker inside a native tool-result blob
/// cleaned in place (see [`MEDIA_PLACEHOLDER`]).
///
/// The protected shape is the intersection every adapter treats as an
/// envelope. The compatible adapter alone also recognizes a
/// reasoning-without-calls envelope, but the Anthropic and OpenAI-family
/// adapters would replay that blob as plain text, so protecting it here
/// would leak its markers exactly the way a malformed call list does; it
/// stays under the whole-string rule — a shape the current runtime writer
/// never produces, since it stores plain text and drops reasoning when
/// there are no calls.
fn rewrite_model_visible_text(
    message: &ChatMessage,
    rewrite: impl Fn(&str) -> (String, usize),
) -> (String, usize) {
    let parsed_envelope = if message.role == "assistant" {
        serde_json::from_str::<serde_json::Value>(&message.content).ok()
    } else {
        None
    };
    let Some(mut envelope) = parsed_envelope else {
        return rewrite(&message.content);
    };
    if !envelope.get("tool_calls").is_some_and(|tool_calls| {
        serde_json::from_value::<Vec<zeroclaw_api::model_provider::ToolCall>>(tool_calls.clone())
            .is_ok()
    }) {
        return rewrite(&message.content);
    }
    let Some(content) = envelope.get("content").and_then(serde_json::Value::as_str) else {
        // A null or absent `content` carries nothing model-visible to
        // rewrite; the envelope must stay byte-identical.
        return (message.content.clone(), 0);
    };
    let (rewritten, n) = rewrite(content);
    if n == 0 {
        // Nothing changed: hand back the original bytes, never a
        // re-serialization whose key order or escaping could drift.
        return (message.content.clone(), 0);
    }
    envelope["content"] = serde_json::Value::String(rewritten);
    (envelope.to_string(), n)
}

/// Strip every media marker from the model-visible text of one message,
/// for the text-only degrade path: all marker kinds, all roles.
///
/// Same contract as [`sanitize_image_markers`] and
/// [`sanitize_audio_markers`], applied to a single message: an assistant
/// tool-call envelope is rewritten field-wise (only its `content` string,
/// through `rewrite_model_visible_text`) so signed `reasoning_content` and
/// `tool_calls[].extra_content` replay byte-for-byte, and every other
/// message is rewritten as a whole string. [`strip_media_markers`] stays
/// the plain-string form for callers that hold text rather than a history
/// message.
pub fn strip_media_markers_model_visible(message: &ChatMessage) -> String {
    rewrite_model_visible_text(message, |text| {
        let out = strip_media_markers(text);
        let n = usize::from(out != text);
        (out, n)
    })
    .0
}

/// Strip loadable audio markers (see `strip_unplayable_audio_markers`)
/// across every message in `messages`, logging one degradation warning when
/// any are removed. Returns the input borrowed when no candidate marker is
/// present (the common, allocation-free path) or an owned rebuilt vector
/// otherwise.
///
/// Assistant messages in native tool-call history are rewritten field-wise —
/// only their `content` string, through `rewrite_model_visible_text` — so
/// signed reasoning inside the envelope survives the seam.
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
            let (content, n) = rewrite_model_visible_text(m, strip_unplayable_audio_markers);
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

/// Matches image markers, capturing the payload for the inline-reference
/// check. Built from [`IMAGE_MARKER_PREFIX`] so this seam helper and
/// [`parse_image_markers`] agree on exactly which markers are image markers;
/// the match is case-sensitive for the same reason.
static IMAGE_MARKER_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"{}([^\]]*)\]",
        regex::escape(IMAGE_MARKER_PREFIX)
    ))
    .expect("static image-marker regex must compile")
});

/// Replace image markers whose payload is a loadable reference other than an
/// inline `data:` URI (a filesystem path or an `http(s)://` URL) with the same
/// [`MEDIA_PLACEHOLDER`] the degrade path uses, returning the rewritten text
/// and the number of markers replaced.
///
/// A `data:` URI is inline content: it has either already passed the
/// normalizer's size and MIME checks or will hit the provider adapter's
/// structural check, so it stays. Non-loadable payloads are prose and stay
/// verbatim, mirroring how [`parse_image_markers`] preserves them. Runs over
/// the raw string so a marker embedded in a native tool-result JSON blob is
/// cleaned in place; see [`MEDIA_PLACEHOLDER`] for why the surrounding object
/// stays valid.
fn strip_undeliverable_image_markers(text: &str) -> (String, usize) {
    let mut stripped = 0usize;
    let out = IMAGE_MARKER_RE.replace_all(text, |caps: &regex::Captures<'_>| {
        let payload = collapse_wrapped_marker(&caps[1]);
        if !payload.is_empty()
            && is_loadable_image_reference(&payload)
            && !payload.starts_with("data:")
        {
            stripped += 1;
            MEDIA_PLACEHOLDER.to_string()
        } else {
            // Preserve inline data URIs and non-loadable markers verbatim.
            caps[0].to_string()
        }
    });
    (out.into_owned(), stripped)
}

/// Strip image markers that cannot be delivered inline (see
/// `strip_undeliverable_image_markers`) across every message, logging one
/// degradation warning when any are removed. Returns the input borrowed when
/// no image marker is present (the common, allocation-free path) or an owned
/// rebuilt vector otherwise.
///
/// Assistant messages in native tool-call history are rewritten field-wise —
/// only their `content` string, through `rewrite_model_visible_text` — so
/// signed reasoning inside the envelope survives the seam.
///
/// This is the fail-closed backstop on one-shot dispatch seams that send
/// stored history without the full multimodal preparation: a filesystem path
/// or URL image marker can no longer reach a provider adapter, whichever
/// route the history took. Inline `data:` URIs and non-loadable prose markers
/// pass through; callers that want the turn's images delivered run the full
/// normalizer before they reach this point.
pub fn sanitize_image_markers(messages: &[ChatMessage]) -> Cow<'_, [ChatMessage]> {
    if !messages
        .iter()
        .any(|m| IMAGE_MARKER_RE.is_match(&m.content))
    {
        return Cow::Borrowed(messages);
    }

    let mut stripped = 0usize;
    let rebuilt: Vec<ChatMessage> = messages
        .iter()
        .map(|m| {
            let (content, n) = rewrite_model_visible_text(m, strip_undeliverable_image_markers);
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
            "multimodal: stripped image marker(s) with a path or URL reference on a one-shot dispatch seam; no size or content validation runs there, so the reference was replaced with a placeholder instead of being sent to the model as text"
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

pub(crate) fn is_prompt_tool_result_content(content: &str) -> bool {
    content.trim_start().starts_with("[Tool results]")
}

pub(crate) fn is_prompt_tool_result_message(message: &ChatMessage) -> bool {
    message.role == "user" && is_prompt_tool_result_content(&message.content)
}

fn is_tool_result_carrier(message: &ChatMessage) -> bool {
    message.role == "tool" || is_prompt_tool_result_message(message)
}

/// Indices of every tool-result carrier in the CURRENT user turn: the
/// carriers after the most recent user message that is not itself a
/// prompt-mode tool-result carrier. If no such user message exists, every
/// tool-result carrier qualifies. Tool images stay live for the user turn
/// that produced them; they are stripped once the next user message
/// arrives, so a tool image is never replayed on every later request
/// forever.
fn current_turn_tool_result_indices(messages: &[ChatMessage]) -> HashSet<usize> {
    let current_turn_start = messages
        .iter()
        .rposition(|message| message.role == "user" && !is_prompt_tool_result_message(message));

    let mut indices = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        if current_turn_start.is_some_and(|start| index < start) {
            continue;
        }
        if is_tool_result_carrier(message) {
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
    if is_tool_result_carrier(message) {
        return current_turn_tool_result_indices.contains(&index);
    }

    message.role == "user"
}

/// How multimodal preparation will treat the `[IMAGE:...]` markers in a
/// message at its position in the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageMarkerDisposition {
    /// Loadable markers become provider image blocks: user messages and the
    /// tool-result carriers in the current user turn.
    Normalized,
    /// Markers are stripped before dispatch: tool-result carriers before the
    /// current user turn.
    Stripped,
    /// Content is dispatched verbatim as text: system and assistant messages.
    Literal,
}

/// One disposition per message, computed with the same predicates preparation
/// uses (`is_tool_result_carrier`, `current_turn_tool_result_indices`,
/// `should_normalize_message_images`). Dispositions do not model the
/// config-dependent image limits (`max_images`, `max_image_turns`), which can
/// strip or cap images preparation would otherwise dispatch.
pub fn image_marker_dispositions(messages: &[ChatMessage]) -> Vec<ImageMarkerDisposition> {
    let current_turn_indices = current_turn_tool_result_indices(messages);
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            if should_normalize_message_images(index, message, &current_turn_indices) {
                ImageMarkerDisposition::Normalized
            } else if is_tool_result_carrier(message) {
                ImageMarkerDisposition::Stripped
            } else {
                ImageMarkerDisposition::Literal
            }
        })
        .collect()
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
    current_turn_tool_result_indices: &HashSet<usize>,
) -> ChatMessage {
    if is_tool_result_carrier(message) && !current_turn_tool_result_indices.contains(&index) {
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

    let normalized =
        normalize_image_references(&refs, config, max_bytes, remote_client, ctx, cache).await;
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

    let current_turn_tool_indices = current_turn_tool_result_indices(messages);
    let total_images =
        count_image_markers_with_current_turn_tool_results(messages, &current_turn_tool_indices);

    if total_images == 0 {
        return Ok(PreparedMessages {
            messages: messages
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    replay_message_without_stale_tool_images(
                        index,
                        message,
                        &current_turn_tool_indices,
                    )
                })
                .collect(),
            contains_images: false,
        });
    }

    // Normalize every image marker first, then enforce the per-request image
    // cap further below based only on images that *successfully* normalize.
    // Trimming the oldest images *before* normalization is unsafe: a newer
    // image ref that fails to load would evict an older valid one that could
    // still have been sent (see `skipped_images_do_not_consume_image_budget`).
    // The post-normalization cap keeps the most recent successful images and
    // prevents conversations from sticking once the cumulative count crosses
    // the threshold, so no pre-normalization trim is needed here.
    let remote_client = build_runtime_proxy_client_with_timeouts("model_provider.ollama", 30, 10);
    let current_turn_tool_indices = current_turn_tool_result_indices(messages);

    let mut normalized_messages = Vec::with_capacity(messages.len());
    let mut has_successful_images = false;
    for (index, message) in messages.iter().enumerate() {
        if !should_normalize_message_images(index, message, &current_turn_tool_indices) {
            normalized_messages.push(replay_message_without_stale_tool_images(
                index,
                message,
                &current_turn_tool_indices,
            ));
            continue;
        }

        if message.role == "tool"
            && let Some((prepared, contains_images)) = normalize_native_tool_result_json(
                &message.content,
                config,
                max_bytes,
                &remote_client,
                &ImageNormalizeCtx {
                    message_index: index,
                    role: &message.role,
                },
                cache.as_deref_mut(),
            )
            .await
        {
            normalized_messages.push(ChatMessage {
                role: message.role.clone(),
                content: prepared,
            });
            has_successful_images |= contains_images;
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
        )
        .await;
        let content = compose_multimodal_content(
            &cleaned_text,
            &normalized.data_uris,
            normalized.skipped_count,
            refs.len(),
        );
        has_successful_images |= !normalized.data_uris.is_empty();
        normalized_messages.push(ChatMessage {
            role: message.role.clone(),
            content,
        });
    }

    // Apply age-based trimming when configured: strip images from user
    // messages older than `max_image_turns` real user turns back from the end
    // of history. Prompt-mode tool-result carriers never count as turns and
    // are never aged out here; the stale-tool rule above owns their lifetime.
    // `max_image_turns == 0` means disabled — no age trimming.
    let age_trimmed = if config.max_image_turns > 0 {
        let before = count_image_markers(&normalized_messages);
        let trimmed = trim_images_by_age(&normalized_messages, config.max_image_turns);
        let after = count_image_markers(&trimmed);
        if after < before {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "max_image_turns": config.max_image_turns,
                        "images_before": before,
                        "images_after": after,
                        "images_dropped": before - after,
                    })),
                "multimodal: age-trimmed old images from conversation history"
            );
        }
        trimmed
    } else {
        normalized_messages
    };

    // Apply the per-request image cap after normalization so failed image refs
    // do not consume budget and evict older images that could still be sent.
    let capped_messages = if has_successful_images && count_image_markers(&age_trimmed) > max_images
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "images_after_normalization": count_image_markers(&age_trimmed),
                    "max_images": max_images,
                })),
            "multimodal: post-normalization image cap exceeded — trimming oldest images"
        );
        trim_old_images(&age_trimmed, max_images)
    } else {
        age_trimmed
    };

    Ok(PreparedMessages {
        contains_images: count_image_markers(&capped_messages) > 0,
        messages: capped_messages,
    })
}

/// Strip image markers from user messages older than `max_turns` conversation
/// turns, where a turn opens at a user message that is not a prompt-mode
/// tool-result carrier. Carriers never advance the turn count and are never
/// stripped here; the stale-tool rule and the post-normalization image cap
/// govern their lifetime, as the `max_image_turns` schema doc documents.
fn trim_images_by_age(messages: &[ChatMessage], max_turns: usize) -> Vec<ChatMessage> {
    // Count real user turns from the end to find the cutoff index.
    let mut user_turn_count = 0usize;
    let mut cutoff = 0usize; // messages at index < cutoff are "too old"
    for (i, m) in messages.iter().enumerate().rev() {
        if m.role == "user" && !is_prompt_tool_result_message(m) {
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
            if i < cutoff && m.role == "user" && !is_prompt_tool_result_message(m) {
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

/// Strip image markers from older messages (oldest first) until the total image
/// count is within `max_images`. Keeps the text content of each message.
///
/// Eviction is per image, not per message: exactly `total - max_images` images
/// are dropped, so a message holding more images than the budget allows keeps
/// its newest ones instead of losing all of them.
fn trim_old_images(messages: &[ChatMessage], max_images: usize) -> Vec<ChatMessage> {
    let current_turn_tool_indices = current_turn_tool_result_indices(messages);
    // Find which messages (by index) contain images, oldest first.
    let image_positions: Vec<(usize, usize)> = messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            should_normalize_message_images(*index, message, &current_turn_tool_indices)
        })
        .filter_map(|(i, m)| {
            let count = parse_image_markers(&m.content).1.len();
            if count > 0 { Some((i, count)) } else { None }
        })
        .collect();

    // Determine how many images to drop (from the oldest messages).
    let total: usize = image_positions.iter().map(|(_, c)| c).sum();
    let mut to_drop = total.saturating_sub(max_images);

    // Record how many images to drop per message, oldest first. A message is
    // only partially trimmed when it holds more images than remain to drop:
    // marking the whole message would evict images the budget still allows and
    // leave the request under `max_images` (a single message holding more than
    // `max_images` would otherwise lose all of them).
    let mut drop_counts = std::collections::HashMap::new();
    for &(idx, count) in &image_positions {
        if to_drop == 0 {
            break;
        }
        let drop_here = to_drop.min(count);
        drop_counts.insert(idx, drop_here);
        to_drop -= drop_here;
    }

    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let Some(&drop_here) = drop_counts.get(&i) else {
                return replay_message_without_stale_tool_images(i, m, &current_turn_tool_indices);
            };

            trim_message_images(m, drop_here)
        })
        .collect()
}

/// Drop the `drop_here` oldest image markers from `text`, keeping the newest.
fn trim_image_markers(text: &str, drop_here: usize) -> String {
    let (cleaned, refs) = parse_image_markers(text);
    // Newest images within the message survive, matching the oldest-first
    // eviction order across messages.
    let retained = refs.get(drop_here..).unwrap_or(&[]);
    if retained.is_empty() {
        if cleaned.trim().is_empty() {
            "[image removed from history]".to_string()
        } else {
            cleaned
        }
    } else {
        compose_multimodal_message(&cleaned, retained)
    }
}

/// Apply [`trim_image_markers`] to a message, keeping a native tool-result JSON
/// envelope intact.
///
/// A `role = "tool"` message may carry a serialized `{"tool_call_id": ..,
/// "content": ..}` object. Trimming the serialized form would strip markers out
/// of the JSON *and* append the retained ones after the closing brace, leaving
/// text that no longer parses — the provider serializers then lose
/// `tool_call_id` and cannot emit a native tool result. Unwrap first, trim the
/// inner `content`, and re-serialize with the rest of the envelope untouched,
/// mirroring [`strip_tool_result_image_markers`] and
/// [`normalize_native_tool_result_json`].
fn trim_message_images(message: &ChatMessage, drop_here: usize) -> ChatMessage {
    if message.role == "tool"
        && let Ok(serde_json::Value::Object(mut obj)) =
            serde_json::from_str::<serde_json::Value>(&message.content)
        && let Some(serde_json::Value::String(inner)) = obj.get("content").cloned()
    {
        let trimmed = trim_image_markers(&inner, drop_here);
        obj.insert("content".to_string(), serde_json::Value::String(trimmed));
        return ChatMessage {
            role: message.role.clone(),
            content: serde_json::Value::Object(obj).to_string(),
        };
    }

    ChatMessage {
        role: message.role.clone(),
        content: trim_image_markers(&message.content, drop_here),
    }
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
                // Truncate the raw reference so we don't dump a full base64
                // payload into the log, but keep enough to identify the source.
                let marker_preview: String = reference.chars().take(120).collect();
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
        _ => None,
    }
}

async fn normalize_image_reference(
    source: &str,
    config: &MultimodalConfig,
    max_bytes: usize,
    remote_client: &Client,
    cache: Option<&mut LocalImageCache>,
) -> anyhow::Result<String> {
    if source.starts_with("data:") {
        return normalize_data_uri(source, max_bytes);
    }

    if source.starts_with("http://") || source.starts_with("https://") {
        if !config.allow_remote_fetch {
            return Err(MultimodalError::RemoteFetchDisabled {
                input: source.to_string(),
            }
            .into());
        }

        return normalize_remote_image(source, max_bytes, remote_client).await;
    }

    match cache {
        Some(c) => normalize_local_image_cached(source, max_bytes, c).await,
        None => normalize_local_image(source, max_bytes).await,
    }
}

fn normalize_data_uri(source: &str, max_bytes: usize) -> anyhow::Result<String> {
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

    Ok(format!("data:{mime};base64,{}", STANDARD.encode(decoded)))
}

async fn normalize_remote_image(
    source: &str,
    max_bytes: usize,
    remote_client: &Client,
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

    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

async fn normalize_local_image(source: &str, max_bytes: usize) -> anyhow::Result<String> {
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

    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

/// Cache-aware local image loader. On a hit (path + metadata unchanged) returns
/// the stored data URI without touching the filesystem. Files under `/uploads/`
/// are content-addressed and treated as immutable — checked once, never re-read.
async fn normalize_local_image_cached(
    source: &str,
    max_bytes: usize,
    cache: &mut LocalImageCache,
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

    #[tokio::test]
    async fn image_marker_dispositions_match_preparation() {
        let temp = tempfile::tempdir().unwrap();
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        let marker = |name: &str| {
            let path = temp.path().join(format!("{name}.png"));
            std::fs::write(&path, png).unwrap();
            format!("[IMAGE:{}]", path.display())
        };

        // Native shape: the tool carrier before the current user turn is
        // Stripped, both tool rounds inside the current turn stay Normalized,
        // and the assistant's marker is Literal (dispatched verbatim).
        let native = vec![
            ChatMessage::system("system"),
            ChatMessage::user(format!("hi {}", marker("native-user-early"))),
            ChatMessage::assistant(format!("yo {}", marker("native-assistant"))),
            ChatMessage::tool(format!("early result {}", marker("native-tool-early"))),
            ChatMessage::user(format!("turn two {}", marker("native-user-current"))),
            ChatMessage::assistant("calling"),
            ChatMessage::tool(format!("round one {}", marker("native-tool-round-one"))),
            ChatMessage::assistant("calling again"),
            ChatMessage::tool(format!("round two {}", marker("native-tool-round-two"))),
        ];
        assert_eq!(
            image_marker_dispositions(&native),
            vec![
                ImageMarkerDisposition::Literal,    // system
                ImageMarkerDisposition::Normalized, // user message
                ImageMarkerDisposition::Literal,    // assistant: verbatim text
                ImageMarkerDisposition::Stripped,   // tool carrier before the current turn
                ImageMarkerDisposition::Normalized, // user message
                ImageMarkerDisposition::Literal,    // assistant
                ImageMarkerDisposition::Normalized, // current turn, first tool round
                ImageMarkerDisposition::Literal,    // assistant
                ImageMarkerDisposition::Normalized, // current turn, second tool round
            ]
        );

        // Prompt-mode shape: a `[Tool results]` carrier before the current
        // turn is Stripped, and the same carriers inside the current turn are
        // Normalized without opening a new turn.
        let prompt_mode = vec![
            ChatMessage::user(format!(
                "[Tool results]\nearly carrier {}",
                marker("prompt-early")
            )),
            ChatMessage::user(format!("turn {}", marker("prompt-user"))),
            ChatMessage::assistant("called"),
            ChatMessage::user(format!(
                "[Tool results]\ncarrier one {}",
                marker("prompt-carrier-one")
            )),
            ChatMessage::user(format!(
                "[Tool results]\ncarrier two {}",
                marker("prompt-carrier-two")
            )),
        ];
        assert_eq!(
            image_marker_dispositions(&prompt_mode),
            vec![
                ImageMarkerDisposition::Stripped, // carrier before the current turn
                ImageMarkerDisposition::Normalized, // the real user message
                ImageMarkerDisposition::Literal,  // assistant
                ImageMarkerDisposition::Normalized, // carrier inside the current turn
                ImageMarkerDisposition::Normalized, // carrier inside the current turn
            ]
        );

        // Dispositions do not model the config-dependent cap or age trim, so
        // the fixture stays under the cap and the age trim stays off; what
        // preparation does to each message must then match its disposition.
        let config = MultimodalConfig {
            max_images: 8,
            ..Default::default()
        };

        for (name, history) in [("native", &native), ("prompt_mode", &prompt_mode)] {
            let dispositions = image_marker_dispositions(history);
            let prepared = prepare_messages_for_provider(history, &config)
                .await
                .unwrap();
            assert_eq!(prepared.messages.len(), history.len());

            for (index, (original, disposition)) in
                history.iter().zip(dispositions.iter()).enumerate()
            {
                let content = &prepared.messages[index].content;
                let inline_image = content.contains("data:image/png;base64,");
                let had_marker = original.content.contains(".png");
                match disposition {
                    ImageMarkerDisposition::Normalized => {
                        assert!(
                            inline_image,
                            "{name}[{index}] Normalized must dispatch an image block"
                        );
                        assert!(
                            !content.contains(".png"),
                            "{name}[{index}] Normalized must not replay the raw path"
                        );
                    }
                    ImageMarkerDisposition::Stripped => {
                        assert!(
                            !inline_image,
                            "{name}[{index}] Stripped must not dispatch an image block"
                        );
                        assert!(
                            !content.contains(".png"),
                            "{name}[{index}] Stripped must drop the marker"
                        );
                    }
                    ImageMarkerDisposition::Literal => {
                        assert!(
                            !inline_image,
                            "{name}[{index}] Literal must not dispatch an image block"
                        );
                        if had_marker {
                            assert!(
                                content.contains(".png"),
                                "{name}[{index}] Literal must replay the marker verbatim"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn image_marker_summary_matches_parse_image_markers() {
        let placeholder = "[IMAGE:...]";
        let content = format!(
            "  see [IMAGE:/tmp/a.png] plus {placeholder} and [IMAGE:/tmp/wrapped-\nlong.png] ok  "
        );
        let (cleaned, refs) = parse_image_markers(&content);
        let summary = image_marker_summary(&content);

        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0], "/tmp/a.png");
        assert_eq!(refs[1], "/tmp/wrapped-long.png");
        assert_eq!(summary.image_refs, refs.len());

        // Every byte the scanner keeps as text, placeholder included, without
        // the trim parse applies to its cleaned string.
        let expected_text = format!("  see  plus {placeholder} and  ok  ");
        assert_eq!(summary.text_bytes, expected_text.len());
        assert_eq!(summary.text_bytes, cleaned.len() + 4);
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

        assert!(cache.should_report_failure(&reference, "invalid_marker"));

        let (stored_reference, stored_kind) = cache.reported_failures.front().unwrap();
        assert_eq!(stored_reference, &image_failure_reference_key(&reference));
        assert_eq!(stored_reference.len(), 32);
        assert_eq!(*stored_kind, "invalid_marker");
    }

    #[tokio::test]
    async fn cached_preparation_retries_and_resets_missing_image_failure() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("restored.png");
        let reference = image_path.to_string_lossy().to_string();
        let messages = vec![ChatMessage::user(format!("Look [IMAGE:{reference}]"))];
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

        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();
        let restored = prepare_messages_for_provider_cached(&messages, &config, &mut cache)
            .await
            .unwrap();
        assert!(restored.contains_images);
        assert!(
            restored.messages[0]
                .content
                .contains("data:image/png;base64,")
        );
        assert!(cache.reported_failures.is_empty());

        std::fs::remove_file(&image_path).unwrap();
        let missing_again = prepare_messages_for_provider_cached(&messages, &config, &mut cache)
            .await
            .unwrap();
        assert!(!missing_again.contains_images);
        assert_eq!(cache.reported_failures.len(), 1);
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

    #[test]
    fn strip_media_markers_replaces_image_local_path() {
        let input = "Look at [IMAGE:/zeroclaw-data/workspace/telegram_files/photo_1.jpg]";
        assert_eq!(
            strip_media_markers(input),
            format!("Look at {MEDIA_PLACEHOLDER}")
        );
    }

    #[test]
    fn strip_media_markers_replaces_image_data_uri() {
        let input = "Inline [IMAGE:data:image/png;base64,abcd]";
        assert_eq!(
            strip_media_markers(input),
            format!("Inline {MEDIA_PLACEHOLDER}")
        );
    }

    #[test]
    fn strip_media_markers_replaces_all_supported_kinds() {
        // Mirrors `ATTACHMENT_KINDS` in
        // `crates/zeroclaw-channels/src/util.rs`, which is the source of
        // truth for which marker spellings inbound channels can produce.
        let input = "[IMAGE:/a.jpg] [PHOTO:/b.jpg] [DOCUMENT:/c.pdf] [FILE:/d.zip] [VIDEO:/e.mp4] [VOICE:/f.ogg] [AUDIO:/g.wav]";
        let expected = [MEDIA_PLACEHOLDER; 7].join(" ");
        assert_eq!(strip_media_markers(input), expected);
    }

    #[test]
    fn strip_media_markers_is_case_insensitive() {
        // Channel parsers uppercase the kind before comparing, so by the time
        // a marker reaches conversation history it is normally upper-case —
        // but accept lower/mixed case too so we don't depend on that
        // invariant downstream.
        let input = "[image:/a.jpg] [Photo:/b.jpg] [video:/c.mp4]";
        let expected = [MEDIA_PLACEHOLDER; 3].join(" ");
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
            format!("Use [TODO:foo] and [NOTE:bar] but replace {MEDIA_PLACEHOLDER}")
        );
    }

    #[test]
    fn media_placeholder_is_prose_not_a_marker() {
        // A model may copy the placeholder from its input into a reply, so it
        // must not be anything the marker grammar recognises: no bracket
        // span, and nothing the strip paths would rewrite again.
        assert!(!MEDIA_PLACEHOLDER.contains('['));
        assert!(!MEDIA_PLACEHOLDER.contains(']'));
        assert!(!MEDIA_PLACEHOLDER.contains(':'));
        assert!(!MEDIA_PLACEHOLDER.contains(['"', '\\']));
        assert_eq!(strip_media_markers(MEDIA_PLACEHOLDER), MEDIA_PLACEHOLDER);
        assert_eq!(
            strip_unplayable_audio_markers(MEDIA_PLACEHOLDER),
            (MEDIA_PLACEHOLDER.to_string(), 0)
        );
        assert!(parse_image_markers(MEDIA_PLACEHOLDER).1.is_empty());
    }

    // ── loadable audio markers degrade; other media kinds keep their paths ──

    #[test]
    fn strip_unplayable_audio_markers_replaces_loadable_audio_path() {
        let (out, n) = strip_unplayable_audio_markers("hear this [AUDIO:/tmp/clip.wav] now");
        assert_eq!(out, format!("hear this {MEDIA_PLACEHOLDER} now"));
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
            format!(
                "[PHOTO:/a.jpg] [DOCUMENT:/b.pdf] [FILE:/c.zip] [VIDEO:/d.mp4] \
                 {MEDIA_PLACEHOLDER} {MEDIA_PLACEHOLDER}"
            )
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
        assert_eq!(out, format!("[IMAGE:/a.png] and {MEDIA_PLACEHOLDER}"));
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
        assert_eq!(out, MEDIA_PLACEHOLDER);
        assert_eq!(n, 1);
    }

    #[test]
    fn strip_unplayable_audio_markers_handles_data_uri_and_url() {
        let (out, n) = strip_unplayable_audio_markers(
            "[VOICE:data:audio/ogg;base64,AAAA] and [AUDIO:https://x/y.mp3]",
        );
        assert_eq!(out, format!("{MEDIA_PLACEHOLDER} and {MEDIA_PLACEHOLDER}"));
        assert_eq!(n, 2);
    }

    // ── seam-level image sanitize: paths/URLs drop, inline data URIs stay ──

    #[test]
    fn strip_undeliverable_image_markers_replaces_path_and_url() {
        let (out, n) = strip_undeliverable_image_markers(
            "see [IMAGE:/tmp/shot.png] and [IMAGE:https://x/y.png]",
        );
        assert_eq!(
            out,
            format!("see {MEDIA_PLACEHOLDER} and {MEDIA_PLACEHOLDER}")
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn strip_undeliverable_image_markers_keeps_inline_data_uri() {
        let (out, n) =
            strip_undeliverable_image_markers("see [IMAGE:data:image/png;base64,iVBORw0KGgo=]");
        assert_eq!(out, "see [IMAGE:data:image/png;base64,iVBORw0KGgo=]");
        assert_eq!(n, 0);
    }

    #[test]
    fn strip_undeliverable_image_markers_preserves_non_loadable_payloads() {
        // Prose and placeholder markers are harmless literal text and must
        // survive, mirroring `parse_image_markers`.
        for input in ["[IMAGE:...]", "[IMAGE:<screenshot>]", "[IMAGE:shot.png]"] {
            let (out, n) = strip_undeliverable_image_markers(input);
            assert_eq!(out, input, "should preserve non-loadable marker: {input}");
            assert_eq!(
                n, 0,
                "non-loadable marker must not count as stripped: {input}"
            );
        }
    }

    #[test]
    fn strip_undeliverable_image_markers_is_case_sensitive_like_parse() {
        // `parse_image_markers` matches the prefix literally, so a lowercase
        // kind is prose everywhere downstream; the seam helper must agree or
        // it would rewrite text the in-loop path leaves alone.
        let (out, n) = strip_undeliverable_image_markers("[image:/tmp/shot.png]");
        assert_eq!(out, "[image:/tmp/shot.png]");
        assert_eq!(n, 0);
    }

    #[test]
    fn sanitize_image_markers_borrows_clean_input() {
        let messages = [ChatMessage::user("no markers here")];
        assert!(matches!(
            sanitize_image_markers(&messages),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn sanitize_image_markers_borrows_when_only_inline_data_uris_present() {
        let messages = [ChatMessage::user(
            "see [IMAGE:data:image/png;base64,iVBORw0KGgo=]",
        )];
        let sanitized = sanitize_image_markers(&messages);
        // Rebuilt (the regex matched) but byte-identical content.
        assert_eq!(sanitized[0].content, messages[0].content);
    }

    #[test]
    fn sanitize_image_markers_rewrites_path_markers_in_place() {
        let marker = format!("[{}:{}]", "IMAGE", "/tmp/shot.png");
        let messages = [
            ChatMessage::user("look"),
            ChatMessage::tool(
                serde_json::json!({
                    "content": format!("saved {marker}"),
                    "tool_call_id": "toolu_1",
                })
                .to_string(),
            ),
        ];
        let sanitized = sanitize_image_markers(&messages);
        assert_eq!(sanitized[0].content, "look");
        let parsed: serde_json::Value =
            serde_json::from_str(&sanitized[1].content).expect("tool JSON stays valid");
        assert_eq!(
            parsed["content"],
            format!("saved {MEDIA_PLACEHOLDER}"),
            "the marker inside the native tool-result envelope must be replaced"
        );
        assert_eq!(parsed["tool_call_id"], "toolu_1");
    }

    // The assistant history entry for native tool calls is a JSON envelope
    // (`content` / `tool_calls` / `reasoning_content`). `reasoning_content`
    // carries signed thinking blocks and `tool_calls[].extra_content` carries
    // provider signatures; both must replay byte-for-byte, so the seam
    // rewrites only the envelope's `content` field.
    #[test]
    fn sanitize_image_markers_preserves_signed_reasoning_in_assistant_envelope() {
        let marker = format!("[{}:{}]", "IMAGE", "/tmp/shot.png");
        let reasoning = format!(r#"{{"thinking":"look at {marker} first","signature":"sig_abc"}}"#);
        let tool_calls = serde_json::json!([{
            "id": "toolu_1",
            "name": "shell",
            "arguments": "{}",
            "extra_content": {"google": {"thought_signature": "sig_gemini"}},
        }]);
        let messages = [
            ChatMessage::assistant(
                serde_json::json!({
                    "content": format!("saved {marker}"),
                    "tool_calls": tool_calls.clone(),
                    "reasoning_content": reasoning.clone(),
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "content": format!("saved {marker}"),
                    "tool_call_id": "toolu_1",
                })
                .to_string(),
            ),
        ];
        let sanitized = sanitize_image_markers(&messages);
        let parsed: serde_json::Value = serde_json::from_str(&sanitized[0].content)
            .expect("assistant envelope stays valid JSON");
        assert_eq!(
            parsed["content"],
            format!("saved {MEDIA_PLACEHOLDER}"),
            "the marker in the envelope's content field is replaced"
        );
        assert_eq!(
            parsed["reasoning_content"].as_str(),
            Some(reasoning.as_str()),
            "signed thinking must survive the seam byte-for-byte"
        );
        assert_eq!(
            parsed["tool_calls"], tool_calls,
            "tool calls (including extra_content signatures) must round-trip unchanged"
        );
        let tool_parsed: serde_json::Value =
            serde_json::from_str(&sanitized[1].content).expect("tool JSON stays valid");
        assert_eq!(
            tool_parsed["content"],
            format!("saved {MEDIA_PLACEHOLDER}"),
            "the tool message's marker is still replaced in place"
        );
    }

    // The envelope's only marker lives inside `reasoning_content`, so the
    // `content` rewrite counts zero and the envelope must come back
    // byte-identical. The input is a hand-written non-canonical spelling —
    // `tool_calls` before `content`, a space after a colon, and `\u002f`
    // escapes inside the marker's path — which serde_json would never
    // produce: a re-serialization spells the path with `/` and drops the
    // spaces, so only the zero-rewrite early return can reproduce these
    // exact bytes.
    #[test]
    fn sanitize_image_markers_leaves_assistant_envelope_byte_identical_when_only_reasoning_matches()
    {
        let marker = format!("[{}:{}]", "IMAGE", r"\u002ftmp\u002fshot.png");
        let envelope = format!(
            r#"{{"tool_calls": [{{"id": "toolu_1", "name": "shell", "arguments": "{{}}", "extra_content": {{"google": {{"thought_signature": "sig_gemini"}}}}}}], "content": "running the tool", "reasoning_content": "{{\"thinking\":\"look at {marker} first\",\"signature\":\"sig_abc\"}}"}}"#
        );
        let messages = [ChatMessage::assistant(envelope)];
        let sanitized = sanitize_image_markers(&messages);
        assert_eq!(
            sanitized[0].content, messages[0].content,
            "an envelope whose content is untouched must not be re-serialized"
        );
    }

    // A malformed call list (an element missing `name`/`arguments`) makes
    // every adapter replay the whole blob as plain assistant text, so the
    // seam must sanitize it as plain text too: fail-closed whole-string
    // rewrite, not field-wise protection that would leave a marker inside
    // `reasoning_content` untouched.
    #[test]
    fn sanitize_image_markers_rewrites_whole_string_when_tool_calls_do_not_deserialize() {
        let marker = format!("[{}:{}]", "IMAGE", "/tmp/shot.png");
        let reasoning = format!(r#"{{"thinking":"look at {marker} first","signature":"sig_abc"}}"#);
        let envelope = serde_json::json!({
            "content": "running",
            "tool_calls": [{"id": "x"}],
            "reasoning_content": reasoning,
        })
        .to_string();
        let messages = [ChatMessage::assistant(envelope)];
        let sanitized = sanitize_image_markers(&messages);
        assert!(
            sanitized[0].content.contains(MEDIA_PLACEHOLDER),
            "a malformed call list must fall back to the whole-string rewrite: {}",
            sanitized[0].content
        );
        assert!(
            !sanitized[0].content.contains("/tmp/shot.png"),
            "the marker inside the reasoning field must not survive as a raw path: {}",
            sanitized[0].content
        );
    }

    // `null` is not an envelope either: without a call list to deserialize,
    // the adapters replay the blob as plain text, and the seam must match.
    #[test]
    fn sanitize_image_markers_rewrites_whole_string_when_tool_calls_is_not_an_array() {
        let marker = format!("[{}:{}]", "IMAGE", "/tmp/shot.png");
        let reasoning = format!(r#"{{"thinking":"look at {marker} first","signature":"sig_abc"}}"#);
        let envelope = serde_json::json!({
            "content": "running",
            "tool_calls": null,
            "reasoning_content": reasoning,
        })
        .to_string();
        let messages = [ChatMessage::assistant(envelope)];
        let sanitized = sanitize_image_markers(&messages);
        assert!(
            sanitized[0].content.contains(MEDIA_PLACEHOLDER),
            "a null call list must fall back to the whole-string rewrite: {}",
            sanitized[0].content
        );
        assert!(
            !sanitized[0].content.contains("/tmp/shot.png"),
            "the marker inside the reasoning field must not survive as a raw path: {}",
            sanitized[0].content
        );
    }

    // Audio twin of the image case: the seam rewrites the envelope's
    // `content` field and leaves the signed reasoning untouched.
    #[test]
    fn sanitize_audio_markers_preserves_signed_reasoning_in_assistant_envelope() {
        let marker = format!("[{}:{}]", "AUDIO", "/tmp/clip.wav");
        let reasoning = format!(
            r#"{{"thinking":"listen to {marker} before answering","signature":"sig_abc"}}"#
        );
        let tool_calls = serde_json::json!([{
            "id": "toolu_1",
            "name": "shell",
            "arguments": "{}",
            "extra_content": {"google": {"thought_signature": "sig_gemini"}},
        }]);
        let messages = [
            ChatMessage::assistant(
                serde_json::json!({
                    "content": format!("heard {marker}"),
                    "tool_calls": tool_calls.clone(),
                    "reasoning_content": reasoning.clone(),
                })
                .to_string(),
            ),
            ChatMessage::tool(
                serde_json::json!({
                    "content": format!("heard {marker}"),
                    "tool_call_id": "toolu_1",
                })
                .to_string(),
            ),
        ];
        let sanitized = sanitize_audio_markers(&messages);
        let parsed: serde_json::Value = serde_json::from_str(&sanitized[0].content)
            .expect("assistant envelope stays valid JSON");
        assert_eq!(
            parsed["content"],
            format!("heard {MEDIA_PLACEHOLDER}"),
            "the audio marker in the envelope's content field is replaced"
        );
        assert_eq!(
            parsed["reasoning_content"].as_str(),
            Some(reasoning.as_str()),
            "signed thinking must survive the seam byte-for-byte"
        );
        assert_eq!(
            parsed["tool_calls"], tool_calls,
            "tool calls (including extra_content signatures) must round-trip unchanged"
        );
        let tool_parsed: serde_json::Value =
            serde_json::from_str(&sanitized[1].content).expect("tool JSON stays valid");
        assert_eq!(
            tool_parsed["content"],
            format!("heard {MEDIA_PLACEHOLDER}"),
            "the tool message's audio marker is still replaced in place"
        );
    }

    // Audio twin of the malformed-call-list case: the envelope's call list
    // does not deserialize, so the adapters replay the blob as plain text
    // and the seam rewrites it as plain text, marker inside
    // `reasoning_content` included.
    #[test]
    fn sanitize_audio_markers_rewrites_whole_string_when_tool_calls_do_not_deserialize() {
        let marker = format!("[{}:{}]", "AUDIO", "/tmp/clip.wav");
        let reasoning = format!(
            r#"{{"thinking":"listen to {marker} before answering","signature":"sig_abc"}}"#
        );
        let envelope = serde_json::json!({
            "content": "running",
            "tool_calls": [{"id": "x"}],
            "reasoning_content": reasoning,
        })
        .to_string();
        let messages = [ChatMessage::assistant(envelope)];
        let sanitized = sanitize_audio_markers(&messages);
        assert!(
            sanitized[0].content.contains(MEDIA_PLACEHOLDER),
            "a malformed call list must fall back to the whole-string rewrite: {}",
            sanitized[0].content
        );
        assert!(
            !sanitized[0].content.contains("/tmp/clip.wav"),
            "the marker inside the reasoning field must not survive as a raw path: {}",
            sanitized[0].content
        );
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
        assert!(tool_msg.content.contains(MEDIA_PLACEHOLDER));
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
        std::fs::write(&path, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
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
        assert!(content.contains(MEDIA_PLACEHOLDER));
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
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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

        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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
    async fn prepare_messages_keeps_native_tool_result_json_valid_when_over_the_image_cap() {
        // Regression: partial trimming used to parse markers out of the whole
        // serialized envelope and append the retained ones after the closing
        // brace, so the tool result stopped being JSON and the provider
        // serializers lost `tool_call_id`. Five images against the default cap
        // of four is enough to force a partial trim.
        let temp = tempfile::tempdir().unwrap();
        let mut markers = Vec::new();
        for index in 0..5 {
            let image_path = temp.path().join(format!("shot-{index}.png"));
            std::fs::write(
                &image_path,
                [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
            )
            .unwrap();
            markers.push(format!("[IMAGE:{}]", image_path.display()));
        }

        let native_tool_content = serde_json::json!({
            "tool_call_id": "tc-overflow",
            "tool_name": "screenshot",
            "content": format!("captured five {}", markers.join(" ")),
        })
        .to_string();

        let config = MultimodalConfig::default();
        let prepared =
            prepare_messages_for_provider(&[ChatMessage::tool(native_tool_content)], &config)
                .await
                .expect("preparation should succeed for an over-cap native tool result");

        assert_eq!(prepared.messages[0].role, "tool");
        let value: serde_json::Value = serde_json::from_str(&prepared.messages[0].content)
            .expect("an over-cap tool result must still be valid JSON");

        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc-overflow"),
            "tool_call_id must survive trimming so the provider can emit a native tool result"
        );
        assert_eq!(
            value.get("tool_name").and_then(|v| v.as_str()),
            Some("screenshot"),
            "other envelope metadata must survive trimming"
        );

        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content must remain a JSON string");
        assert!(
            inner.contains("captured five"),
            "surrounding text must survive trimming"
        );

        let (_, refs) = parse_image_markers(inner);
        assert_eq!(
            refs.len(),
            config.max_images,
            "exactly the budgeted images are retained, and they live inside `content`"
        );
        assert!(
            refs.iter()
                .all(|reference| reference.starts_with("data:image/png;base64,")),
            "retained images stay normalized data URIs"
        );
    }

    #[tokio::test]
    async fn prepare_messages_preserves_native_tool_result_json_shape() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("native-tool-result.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        std::fs::write(&stale_path, png).unwrap();
        std::fs::write(&fresh_path, png).unwrap();

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

    #[tokio::test]
    async fn prepare_messages_keeps_tool_image_after_unrelated_tool_call_in_same_turn() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("same-turn-tool-result.png");
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        std::fs::write(&image_path, png).unwrap();

        let image_tool_content = serde_json::json!({
            "tool_call_id": "tc_image",
            "content": format!(
                "generated screenshot [IMAGE:{}]",
                image_path.display().to_string()
            ),
        })
        .to_string();
        let weather_tool_content = serde_json::json!({
            "tool_call_id": "tc_weather",
            "content": "Sunny, 25C".to_string(),
        })
        .to_string();

        let messages = vec![
            ChatMessage::user("Take a screenshot, then check the weather.".to_string()),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "tc_image", "name": "screenshot", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(image_tool_content),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "tc_weather", "name": "weather", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(weather_tool_content),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("tool images stay live for the user turn that produced them");

        assert!(
            prepared.contains_images,
            "an image tool result followed by an unrelated tool result in the same turn must stay normalized"
        );

        let value: serde_json::Value = serde_json::from_str(&prepared.messages[2].content)
            .expect("tool result should remain valid JSON");
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc_image")
        );
        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert!(inner.contains("generated screenshot"));
        // The marker is rewritten in place to wrap the data URI, so the raw
        // filesystem path must be gone while the payload is inline.
        assert!(inner.contains("data:image/png;base64,"));
        assert!(!inner.contains("same-turn-tool-result.png"));
    }

    #[tokio::test]
    async fn prepare_messages_strips_tool_image_after_next_user_message() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("same-turn-tool-result.png");
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        std::fs::write(&image_path, png).unwrap();

        let image_tool_content = serde_json::json!({
            "tool_call_id": "tc_image",
            "content": format!(
                "generated screenshot [IMAGE:{}]",
                image_path.display().to_string()
            ),
        })
        .to_string();
        let weather_tool_content = serde_json::json!({
            "tool_call_id": "tc_weather",
            "content": "Sunny, 25C".to_string(),
        })
        .to_string();

        let messages = vec![
            ChatMessage::user("Take a screenshot, then check the weather.".to_string()),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "tc_image", "name": "screenshot", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(image_tool_content),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "tc_weather", "name": "weather", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::tool(weather_tool_content),
            ChatMessage::user("What did you find?".to_string()),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("stale tool images should strip without loading them");

        assert!(
            !prepared.contains_images,
            "once the next user message arrives the turn's tool images are stale"
        );

        let value: serde_json::Value = serde_json::from_str(&prepared.messages[2].content)
            .expect("stale native tool result should remain valid JSON");
        assert_eq!(
            value.get("tool_call_id").and_then(|v| v.as_str()),
            Some("tc_image")
        );
        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert!(inner.contains("generated screenshot"));
        assert!(!inner.contains("[IMAGE:"));
        assert!(!inner.contains("data:image"));
        assert!(!inner.contains("same-turn-tool-result.png"));

        let weather_value: serde_json::Value = serde_json::from_str(&prepared.messages[4].content)
            .expect("the text-only tool result should remain valid JSON");
        assert_eq!(
            weather_value.get("content").and_then(|v| v.as_str()),
            Some("Sunny, 25C")
        );
    }

    #[tokio::test]
    async fn prepare_messages_keeps_prompt_mode_tool_images_within_turn() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("same-turn-prompt-tool-result.png");
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        std::fs::write(&image_path, png).unwrap();

        let messages = vec![
            ChatMessage::user("Compare these two.".to_string()),
            ChatMessage::user(format!(
                "[Tool results]\n<tool_result name=\"image_gen\">Generated [IMAGE:{}]</tool_result>",
                image_path.display()
            )),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"weather\">Sunny, 25C</tool_result>"
                    .to_string(),
            ),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("prompt-mode tool images stay live for the user turn that produced them");

        assert!(
            prepared.contains_images,
            "an earlier prompt-mode tool-result carrier in the same turn must stay normalized"
        );

        assert!(prepared.messages[1].content.contains("[Tool results]"));
        assert!(prepared.messages[1].content.contains("Generated"));
        assert!(
            prepared.messages[1]
                .content
                .contains("data:image/png;base64,")
        );
        assert!(
            !prepared.messages[1]
                .content
                .contains("same-turn-prompt-tool-result.png")
        );
        assert!(prepared.messages[2].content.contains("Sunny, 25C"));
    }

    #[tokio::test]
    async fn prepare_messages_keeps_prompt_mode_tool_images_within_turn_under_age_limit() {
        let temp = tempfile::tempdir().unwrap();
        let user_image_path = temp.path().join("age-limit-user-attachment.png");
        let tool_image_path = temp.path().join("age-limit-tool-result.png");
        // Minimal valid PNG (1x1 RGB pixel).
        let png_data = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
            0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(&user_image_path, png_data).unwrap();
        std::fs::write(&tool_image_path, png_data).unwrap();

        let messages = vec![
            ChatMessage::user(format!(
                "Inspect the attached image, then check session information\n[IMAGE:{}]",
                user_image_path.display()
            )),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "tc_image", "name": "image_tool", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::user(format!(
                "[Tool results]\n<tool_result name=\"image_tool\">Generated [IMAGE:{}]</tool_result>",
                tool_image_path.display()
            )),
            ChatMessage::assistant(
                serde_json::json!({
                    "content": "",
                    "tool_calls": [
                        {"id": "tc_session", "name": "session_info", "arguments": "{}"}
                    ]
                })
                .to_string(),
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"session_info\">session id: abc-123</tool_result>"
                    .to_string(),
            ),
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
        assert!(
            prepared.messages[0]
                .content
                .contains("data:image/png;base64,")
        );
        assert!(
            !prepared.messages[0]
                .content
                .contains("age-limit-user-attachment.png")
        );
        assert!(prepared.messages[2].content.contains("[Tool results]"));
        assert!(prepared.messages[2].content.contains("Generated"));
        assert!(
            prepared.messages[2]
                .content
                .contains("data:image/png;base64,")
        );
        assert!(
            !prepared.messages[2]
                .content
                .contains("age-limit-tool-result.png")
        );
        assert!(prepared.messages[4].content.contains("session id: abc-123"));
    }

    #[test]
    fn trim_images_by_age_still_strips_real_user_images_past_the_limit() {
        let messages = vec![
            ChatMessage::user("[IMAGE:/old/user-image.png]".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "Done.".to_string(),
            },
            ChatMessage::user("Next question".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "Answered.".to_string(),
            },
            ChatMessage::user("Follow-up".to_string()),
        ];

        let trimmed = trim_images_by_age(&messages, 1);

        assert_eq!(trimmed[0].content, "[image removed from history]");
        assert_eq!(trimmed[1].content, "Done.");
        assert_eq!(trimmed[2].content, "Next question");
        assert_eq!(trimmed[3].content, "Answered.");
        assert_eq!(trimmed[4].content, "Follow-up");
    }

    #[test]
    fn trim_images_by_age_ignores_prompt_mode_carriers_when_counting() {
        let messages = vec![
            ChatMessage::user("Inspect this screenshot.".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "On it.".to_string(),
            },
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"image_gen\">Generated [IMAGE:/tmp/tool-image.png]</tool_result>"
                    .to_string(),
            ),
            ChatMessage {
                role: "assistant".to_string(),
                content: "Checked.".to_string(),
            },
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"weather\">Sunny, 25C</tool_result>"
                    .to_string(),
            ),
        ];

        let trimmed = trim_images_by_age(&messages, 1);

        assert_eq!(trimmed[0].content, messages[0].content);
        assert_eq!(trimmed[2].content, messages[2].content);
        assert_eq!(trimmed[4].content, messages[4].content);
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
    fn count_image_markers_counts_tool_images_within_turn() {
        // The image tool result sits before a later, text-only tool result in
        // the same user turn; the vision gate must still see its image.
        let messages = vec![
            ChatMessage::user("Create an image, then check the weather.".to_string()),
            ChatMessage::tool("[IMAGE:/tmp/same-turn-tool.png]\nGenerated".to_string()),
            ChatMessage {
                role: "assistant".to_string(),
                content: "Checking the weather next.".to_string(),
            },
            ChatMessage::tool("Sunny, 25C".to_string()),
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
    fn trim_old_images_partially_trims_a_multi_image_message() {
        // A single message has 3 images and the budget is 1, so exactly 2 must
        // be dropped. Evicting the message as a unit would remove all three and
        // leave zero images, spending none of the budget the operator allowed.
        let messages = vec![
            ChatMessage::user(
                "[IMAGE:/tmp/a.png]\n[IMAGE:/tmp/b.png]\n[IMAGE:/tmp/c.png]\nThree pics"
                    .to_string(),
            ),
            ChatMessage::user("Just text, no images".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 1);
        assert_eq!(trimmed.len(), 2);
        // The newest image in the message survives; the two older ones go.
        let (_, refs0) = parse_image_markers(&trimmed[0].content);
        assert_eq!(refs0, vec!["/tmp/c.png".to_string()]);
        assert!(trimmed[0].content.contains("Three pics"));
        // Second message unchanged
        assert_eq!(trimmed[1].content, "Just text, no images");
    }

    #[test]
    fn trim_old_images_drops_exactly_the_overflow() {
        // The invariant the cap exists to enforce: whatever the per-message
        // distribution, the survivors equal the budget rather than undershoot.
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/a.png]\n[IMAGE:/tmp/b.png]\nPair".to_string()),
            ChatMessage::user("[IMAGE:/tmp/c.png]\nSingle".to_string()),
            ChatMessage::user("[IMAGE:/tmp/d.png]\n[IMAGE:/tmp/e.png]\nAnother pair".to_string()),
        ];

        for max_images in 1..=5 {
            let trimmed = trim_old_images(&messages, max_images);
            assert_eq!(
                count_image_markers(&trimmed),
                max_images,
                "max_images={max_images} must keep exactly that many images"
            );
        }
    }

    #[test]
    fn trim_old_images_keeps_the_newest_images_across_messages() {
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/a.png]\n[IMAGE:/tmp/b.png]\nOld".to_string()),
            ChatMessage::user("[IMAGE:/tmp/c.png]\n[IMAGE:/tmp/d.png]\nNew".to_string()),
        ];

        let trimmed = trim_old_images(&messages, 3);

        // Oldest single image evicted; everything newer survives.
        let (_, refs0) = parse_image_markers(&trimmed[0].content);
        assert_eq!(refs0, vec!["/tmp/b.png".to_string()]);
        let (_, refs1) = parse_image_markers(&trimmed[1].content);
        assert_eq!(
            refs1,
            vec!["/tmp/c.png".to_string(), "/tmp/d.png".to_string()]
        );
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
            // Minimal valid PNG (1x1 white pixel)
            let png_data = [
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
                0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
                0x77, 0x53, 0xDE, // 1x1 RGB
                0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
                0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21,
                0xBC, 0x33, // IDAT data + CRC
                0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
                0xAE, 0x42, 0x60, 0x82,
            ];
            std::fs::write(&p, png_data).unwrap();
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
        // Minimal valid PNG (1x1 RGB pixel).
        let png_data = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
            0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        // Nine distinct valid image files across nine user messages, max 4.
        let mut messages = Vec::new();
        for i in 0..9 {
            let p = temp.path().join(format!("img{i}.png"));
            std::fs::write(&p, png_data).unwrap();
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
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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
    async fn skipped_images_do_not_consume_image_budget() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("older-valid.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

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
            .expect("broken image should not evict an older valid image");

        assert!(result.contains_images);
        assert!(
            result.messages[0]
                .content
                .contains("data:image/png;base64,")
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
