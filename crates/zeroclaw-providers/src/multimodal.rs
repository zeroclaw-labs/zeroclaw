use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Client;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use zeroclaw_api::media::{
    PROVIDER_IMAGE_MIME_TYPES, image_mime_from_extension, image_mime_from_magic,
    is_provider_image_mime,
};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_config::schema::{MultimodalConfig, build_runtime_proxy_client_with_timeouts};

const IMAGE_MARKER_PREFIX: &str = "[IMAGE:";

/// Maximum byte length for a single image marker candidate, enforced before any
/// allocation or owned copy is made.
///
/// This is a hard sanity guard enforced in `parse_image_markers` before
/// `collapse_wrapped_marker` or any other owned-copy operation runs. A marker
/// candidate whose UTF-8 byte length exceeds this ceiling is treated as
/// non-loadable and preserved verbatim in the cleaned output, bypassing all
/// downstream validation and normalization.
///
/// The ceiling is deliberately set well above any legitimate configured
/// `max_image_size_mb` (which clamps at 20 MiB decoded → ~27 MB base64 encoded)
/// to avoid false rejections while still bounding parser-internal allocations
/// against pathological input.
const MAX_IMAGE_MARKER_BYTES: usize = 50 * 1024 * 1024; // 50 MiB

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
/// generous enough to never reject a valid marker while still bounding the
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
    /// Counts candidate bytes the parser took ownership of (one add per
    /// collected `refs` entry). Counting and selection passes must run without
    /// owning candidate bodies — only the chosen set may materialize — so a
    /// production-path test can observe this counter to prove no pass before
    /// the cap decision collected attacker-sized spans.
    static CANDIDATE_OWNERSHIP_BYTES: std::sync::atomic::AtomicUsize;
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

/// Records `bytes` of candidate ownership against the ambient counter, if a
/// test installed one.
#[cfg(test)]
fn record_candidate_ownership(bytes: usize) {
    let _ = CANDIDATE_OWNERSHIP_BYTES.try_with(|owned| {
        owned.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
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

const REJECTED_IMAGE_MARKER_NOTE: &str = "[image omitted: image marker exceeds safety limit]";

/// Classify a marker span without owning its attacker-sized body.
///
/// The regular parser collapses line wrapping before classifying a reference,
/// so inspect a small collapsed prefix here as well. Every supported absolute
/// reference shape is identifiable from this bounded prefix. The limit covers
/// the longest classification decision any legal reference needs — a UNC path
/// `\\<253-byte DNS hostname>\<share>` requires 257 bytes to decide, so the
/// prefix is sized with margin rather than exactly.
fn marker_span_is_loadable(raw: &str) -> bool {
    const PREFIX_LIMIT: usize = 512;

    let mut prefix = String::with_capacity(PREFIX_LIMIT);
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
        if prefix.len().saturating_add(ch.len_utf8()) > PREFIX_LIMIT {
            break;
        }
        prefix.push(ch);
    }

    is_loadable_image_reference(prefix.trim())
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
    trim_string_in_place(out)
}

fn trim_string_in_place(mut value: String) -> String {
    let start = value.len() - value.trim_start().len();
    let end = value.trim_end().len();
    let start = start.min(end);
    value.truncate(end);
    if start > 0 {
        value.drain(..start);
    }
    value
}

/// True when `content` holds an image marker, terminated or not.
///
/// This is how a provider adapter tells *residue of this crate's own marker
/// normalization* from a data URI the author wrote deliberately. An
/// Bounded unterminated markers are copied through by
/// [`parse_image_markers`] verbatim, prefix included, so the prefix is present
/// in both the input and cleaned output whenever residue is possible. An
/// over-ceiling unterminated marker is replaced by a fixed note instead.
pub(crate) fn carries_image_marker(content: &str) -> bool {
    content.contains(IMAGE_MARKER_PREFIX)
}

#[derive(Default)]
struct ParsedImageMarkers {
    cleaned: String,
    refs: Vec<String>,
    loadable_count: usize,
    rejected_count: usize,
}

/// What a [`parse_image_markers_inner`] call may own.
///
/// Classification always runs on borrowed spans, so the counting passes in
/// [`ParseMode::Scan`] never materialize candidate bodies — a message full of
/// large-but-under-ceiling markers cannot allocate through a count, no matter
/// how many counting rounds run before the cap is applied. Callers that only
/// rewrite text (stale-history stripping, age trimming) use
/// [`ParseMode::RewriteOnly`] for the same reason: the rewritten text carries
/// no attacker-sized copies either. Only normalization, which must hand the
/// reference downstream, collects candidates — and by then the cap has
/// already chosen the surviving set.
enum ParseMode {
    /// Count loadable and rejected markers. Builds nothing.
    Scan,
    /// Rewrite the text — loadable markers removed, over-ceiling markers
    /// replaced — but collect no candidate bodies.
    RewriteOnly,
    /// Rewrite the text and collect the loadable candidate bodies for
    /// normalization.
    RewriteAndCollect,
}

fn push_rejected_image_marker(cleaned: &mut String) {
    if cleaned.chars().last().is_some_and(|ch| !ch.is_whitespace()) {
        cleaned.push(' ');
    }
    cleaned.push_str(REJECTED_IMAGE_MARKER_NOTE);
}

fn parse_image_markers_inner(content: &str, mode: ParseMode) -> ParsedImageMarkers {
    let materialize = !matches!(mode, ParseMode::Scan);
    let collect_refs = matches!(mode, ParseMode::RewriteAndCollect);
    let mut refs = Vec::new();
    // Do not reserve from the complete untrusted message. A rejected marker
    // can dominate `content.len()`, while the retained output is only the
    // surrounding prose plus a fixed sentinel.
    let mut cleaned = String::new();
    let mut loadable_count = 0usize;
    let mut rejected_count = 0usize;
    let mut cursor = 0usize;

    while let Some(rel_start) = content[cursor..].find(IMAGE_MARKER_PREFIX) {
        let start = cursor + rel_start;
        if materialize {
            cleaned.push_str(&content[cursor..start]);
        }

        let marker_start = start + IMAGE_MARKER_PREFIX.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            let marker_len = content.len() - marker_start;
            if marker_len > MAX_IMAGE_MARKER_BYTES {
                rejected_count += 1;
                if materialize {
                    push_rejected_image_marker(&mut cleaned);
                }
            } else if materialize {
                cleaned.push_str(&content[start..]);
            }
            cursor = content.len();
            break;
        };

        let end = marker_start + rel_end;

        // Bound the candidate span before it is copied.
        //
        // `collapse_wrapped_marker` owns the span (twice, for a line-wrapped
        // marker: a build buffer plus the trimmed result), and every downstream
        // ceiling — the data-URI source check, the base64 encoded check, the
        // decoded-size check — runs on that owned copy. A pathological marker
        // would therefore be materialized several times over before anything
        // rejected it. Checking the borrowed span here keeps the parser's own
        // allocations bounded regardless of what the caller does later.
        //
        // A syntactically loadable over-ceiling reference is rejected here and
        // replaced with fixed text. Placeholder/prose markers retain their
        // historical literal treatment.
        if end - marker_start > MAX_IMAGE_MARKER_BYTES {
            if marker_span_is_loadable(&content[marker_start..end]) {
                rejected_count += 1;
                if materialize {
                    push_rejected_image_marker(&mut cleaned);
                }
            } else if materialize {
                cleaned.push_str(&content[start..=end]);
            }
            cursor = end + 1;
            continue;
        }

        // Classify on the borrowed span here as well — the same boundary the
        // over-ceiling check above relies on — so counting and rewriting
        // never pay for a candidate the cap might not keep. `refs` receives
        // the collapsed body only when the caller will actually normalize it;
        // because both decisions share one classifier, a count can never
        // disagree with the later selection over the same span.
        if !marker_span_is_loadable(&content[marker_start..end]) {
            // Preserve the original marker text (placeholders like
            // `[IMAGE:...]` or `[IMAGE:<path>]` should survive as prose
            // rather than triggering a loader error).
            if materialize {
                cleaned.push_str(&content[start..=end]);
            }
        } else {
            loadable_count += 1;
            if collect_refs {
                let candidate = collapse_wrapped_marker(&content[marker_start..end]);
                #[cfg(test)]
                record_candidate_ownership(candidate.len());
                refs.push(candidate);
            }
        }

        cursor = end + 1;
    }

    if materialize && cursor < content.len() {
        cleaned.push_str(&content[cursor..]);
    }

    ParsedImageMarkers {
        cleaned: if materialize {
            trim_string_in_place(cleaned)
        } else {
            String::new()
        },
        refs,
        loadable_count,
        rejected_count,
    }
}

pub fn parse_image_markers(content: &str) -> (String, Vec<String>) {
    let parsed = parse_image_markers_inner(content, ParseMode::RewriteAndCollect);
    (parsed.cleaned, parsed.refs)
}

pub fn count_image_markers(messages: &[ChatMessage]) -> usize {
    let latest_tool_indices = latest_tool_result_indices(messages);
    count_image_markers_with_latest_tool_results(messages, &latest_tool_indices)
}

/// One non-owning pass over the messages the provider would normalize,
/// returning `(loadable, rejected)` marker counts.
///
/// Both counts feed the fast-path decision at the top of
/// [`prepare_messages_inner`], so they are collected together: two separate
/// counting rounds would double the parse work on every request for no
/// benefit.
fn scan_image_marker_counts_with_latest_tool_results(
    messages: &[ChatMessage],
    latest_tool_result_indices: &HashSet<usize>,
) -> (usize, usize) {
    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            should_normalize_message_images(*index, message, latest_tool_result_indices)
        })
        .map(|(_, message)| {
            let parsed = parse_image_markers_inner(&message.content, ParseMode::Scan);
            (parsed.loadable_count, parsed.rejected_count)
        })
        .fold(
            (0, 0),
            |(loadable, rejected), (more_loadable, more_rejected)| {
                (loadable + more_loadable, rejected + more_rejected)
            },
        )
}

fn count_image_markers_with_latest_tool_results(
    messages: &[ChatMessage],
    latest_tool_result_indices: &HashSet<usize>,
) -> usize {
    scan_image_marker_counts_with_latest_tool_results(messages, latest_tool_result_indices).0
}

pub fn contains_image_markers(messages: &[ChatMessage]) -> bool {
    count_image_markers(messages) > 0
}

pub fn count_user_image_markers(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|message| message.role == "user" && !is_prompt_tool_result_message(message))
        .map(|message| parse_image_markers_inner(&message.content, ParseMode::Scan).loadable_count)
        .sum()
}

pub fn count_latest_user_image_markers(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "user" && !is_prompt_tool_result_message(message))
        .map(|message| parse_image_markers_inner(&message.content, ParseMode::Scan).loadable_count)
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
        .expect("static media-marker regex must compile")
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
    .expect("static audio-marker regex must compile")
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
    let parsed = parse_image_markers_inner(content, ParseMode::RewriteOnly);
    // "No loadable candidates" is not "nothing changed": an over-ceiling
    // marker is rejected rather than collected, and its raw body must not
    // survive this strip. Only a parse that found neither a loadable nor a
    // rejected marker may return the input untouched — placeholder markers
    // are preserved verbatim by the rewrite, so that return equals the input.
    if parsed.loadable_count == 0 && parsed.rejected_count == 0 {
        return content.to_string();
    }

    if parsed.cleaned.trim().is_empty() {
        "[image removed from history]".to_string()
    } else {
        parsed.cleaned
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
    let (total_images, total_rejected) =
        scan_image_marker_counts_with_latest_tool_results(messages, &latest_tool_indices);

    if total_images == 0 && total_rejected == 0 {
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

        let parsed = parse_image_markers_inner(&message.content, ParseMode::RewriteAndCollect);
        let cleaned_text = parsed.cleaned;
        let refs = parsed.refs;
        if refs.is_empty() {
            normalized_messages.push(ChatMessage {
                role: message.role.clone(),
                content: cleaned_text,
            });
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
                let parsed = parse_image_markers_inner(&m.content, ParseMode::RewriteOnly);
                // Same detector contract as `stripped_image_marker_text`: an
                // empty candidate list is not "no change" — a rejected marker
                // rewrites the text without ever producing a candidate.
                if parsed.loadable_count == 0 && parsed.rejected_count == 0 {
                    return m.clone();
                }
                let text = if parsed.cleaned.trim().is_empty() {
                    "[image removed from history]".to_string()
                } else {
                    parsed.cleaned
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
            let count = parse_image_markers_inner(&m.content, ParseMode::Scan).loadable_count;
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
                // The message is already sentenced — its images are being
                // dropped — so rewriting must not also pay to own them.
                let parsed = parse_image_markers_inner(&m.content, ParseMode::RewriteOnly);
                let text = if parsed.cleaned.trim().is_empty() {
                    "[image removed from history]".to_string()
                } else {
                    parsed.cleaned
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
            Ok(data_uri) => data_uris.push(data_uri),
            Err(error) => {
                skipped_count += 1;
                let error_reason = multimodal_error_reason(&error);
                let error_kind = multimodal_error_kind(&error);
                let attrs =
                    skipped_image_log_attrs(ctx, reference, error_kind, error_reason.as_deref());
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
    // Bound the marker length before doing anything that allocates from it.
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
        return Err(MultimodalError::InvalidMarker {
            input: "[data URI]".to_string(),
            reason: "expected data URI payload".to_string(),
        }
        .into());
    };

    let header = &source[..comma_idx];
    let payload = source[comma_idx + 1..].trim();

    if !header.contains(";base64") {
        return Err(MultimodalError::InvalidMarker {
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
    // oversized marker allocate before any limit applies. This runs on the
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
    async fn oversized_data_uri_is_refused_before_base64_decoding() {
        // `STANDARD.decode` sizes its output buffer from the input length, so
        // decoding first and size-checking afterwards lets an oversized marker
        // allocate before any limit applies — on the async preparation task,
        // outside the `spawn_blocking` boundary that isolates pixel decoding.
        //
        // The assertion here is deliberately on the *base64* seam, not the
        // pixel-decode counter. An oversized data URI never reaches the pixel
        // decoder either way: the old ordering decoded the base64, then failed
        // in `validate_size`, so a `DECODE_CALLS == 0` assertion holds both
        // before and after this fix and proves nothing. `BASE64_DECODE_CALLS`
        // is incremented immediately before `STANDARD.decode`, so it is zero
        // only if the encoded-length ceiling refused the payload first.
        // `effective_limits()` returns `(max_images, max_image_size_mb)` — the
        // second value is MEGABYTES, not bytes.
        let cfg = MultimodalConfig::default();
        let (_, max_mb) = cfg.effective_limits();
        let max_bytes = max_mb * 1024 * 1024;

        // Comfortably above `ceil(max_bytes / 3) * 4 + slack`. All-`A` is valid
        // base64, so the old path would have decoded it successfully and only
        // then failed the size check — exactly the allocation being prevented.
        let oversized = "A".repeat(max_bytes / 3 * 4 + 4096);
        let uri = format!("[IMAGE:data:image/png;base64,{oversized}]");

        let (result, base64_decodes) = counting_base64_decodes(async {
            prepare_messages_for_provider(&[ChatMessage::user(uri)], &cfg)
                .await
                .expect("provider preparation does not hard-fail on a refused image")
        })
        .await;

        assert!(
            !result.contains_images,
            "the oversized data URI must be skipped; it reached the prepared payload instead"
        );
        assert_eq!(
            base64_decodes, 0,
            "the encoded-length ceiling must refuse the payload before `STANDARD.decode` runs"
        );

        // A payload inside the ceiling must still decode, so the guard is not
        // simply rejecting everything.
        let valid_uri = format!(
            "[IMAGE:data:image/png;base64,{}]",
            STANDARD.encode(valid_png())
        );
        let (ok_result, ok_base64_decodes) = counting_base64_decodes(async {
            prepare_messages_for_provider(&[ChatMessage::user(valid_uri)], &cfg)
                .await
                .expect("a within-ceiling data URI is accepted")
        })
        .await;
        assert!(
            ok_result.contains_images,
            "a data URI within the ceiling must still reach the provider"
        );
        assert_eq!(
            ok_base64_decodes, 1,
            "the within-ceiling payload must actually be base64-decoded"
        );
    }

    #[tokio::test]
    async fn over_ceiling_marker_is_refused_on_the_production_path() {
        // The parser-boundary counterpart to the test above.
        //
        // That one drives a ~28 MB marker, which is past the configured
        // encoded ceiling but *below* `MAX_IMAGE_MARKER_BYTES`, so it proves
        // the `normalize_data_uri` guard. This one drives a marker past the
        // parser ceiling, which is refused earlier still — before
        // `collapse_wrapped_marker` owns the span at all.
        //
        // Both allocation shapes are covered because they take different paths
        // inside `collapse_wrapped_marker`: the single-line branch copies once,
        // the wrapped branch allocates a build buffer *and* a trimmed copy.
        // Driving them through `prepare_messages_for_provider` rather than the
        // parser directly is what makes this a production-path proof: every
        // counting, trimming, and normalization pass runs.
        let cfg = MultimodalConfig::default();
        let payload = "A".repeat(MAX_IMAGE_MARKER_BYTES + 1);

        for (label, marker_body, terminated) in [
            ("single-line", payload.clone(), true),
            // Newlines force the wrapped branch, the costlier of the two.
            ("wrapped", format!("{payload}\n  continued"), true),
            // The no-closing-bracket path must apply the same bound without
            // copying the complete remainder into the cleaned output.
            ("unterminated", payload.clone(), false),
        ] {
            let message = if terminated {
                format!("before [IMAGE:data:image/png;base64,{marker_body}] after")
            } else {
                format!("before [IMAGE:data:image/png;base64,{marker_body}")
            };

            let ((result, base64_decodes), pixel_decodes) = counting_decodes(async {
                counting_base64_decodes(async {
                    prepare_messages_for_provider(&[ChatMessage::user(message.clone())], &cfg)
                        .await
                        .expect("preparation does not hard-fail on a refused marker")
                })
                .await
            })
            .await;

            assert!(
                !result.contains_images,
                "{label}: an over-ceiling marker must not reach the provider"
            );
            assert_eq!(
                base64_decodes, 0,
                "{label}: rejection must happen before `STANDARD.decode` runs"
            );
            assert_eq!(
                pixel_decodes, 0,
                "{label}: rejection must happen before any pixel decode"
            );

            // The marker is preserved verbatim as prose — the same treatment
            // placeholder markers get — so the message text survives and the
            // rejection never reports the marker through an error path.
            let content = &result.messages[0].content;
            assert!(
                content.starts_with("before "),
                "{label}: surrounding prose must survive the refusal"
            );
            assert!(
                content.contains(REJECTED_IMAGE_MARKER_NOTE),
                "{label}: the refusal must leave a fixed provider-visible note"
            );
            assert!(
                !content.contains("data:image") && !content.contains(&payload[..128]),
                "{label}: the raw marker must not reach provider-visible content"
            );
        }
    }

    #[tokio::test]
    async fn counting_and_cap_selection_do_not_materialize_uncalled_candidates() {
        // Production-path counterpart of the scan tests: every counting,
        // trimming, and selection pass runs, and the ownership counter
        // observes exactly what those passes own. Three under-ceiling markers
        // with a one-image cap means only the newest message's candidate may
        // ever be materialized — the counts, the cap trim, and the
        // stale-replay strip must all classify on borrowed spans.
        //
        // The payloads are over the (clamped, ≤ 20 MiB) encoded ceiling, so
        // the surviving candidate is refused at the normalize entry guard:
        // the test observes ownership, not decode work.
        let payload_bytes = 30 * 1024 * 1024;
        let payload = "A".repeat(payload_bytes);
        let config = MultimodalConfig {
            max_images: 1,
            ..MultimodalConfig::default()
        };
        let messages: Vec<ChatMessage> = (0..3)
            .map(|i| {
                ChatMessage::user(format!(
                    "message {i} [IMAGE:data:image/png;base64,{payload}]"
                ))
            })
            .collect();

        let (((result, base64_decodes), pixel_decodes), owned_bytes) =
            counting_candidate_ownership(async {
                counting_base64_decodes(async {
                    counting_decodes(async {
                        prepare_messages_for_provider(&messages, &config)
                            .await
                            .expect("preparation does not hard-fail on refused candidates")
                    })
                    .await
                })
                .await
            })
            .await;

        assert_eq!(
            base64_decodes, 0,
            "the encoded ceiling must refuse before base64 decoding"
        );
        assert_eq!(pixel_decodes, 0, "nothing reaches a pixel decode");
        // No pass before or during selection owned more than the one chosen
        // candidate body. Before the counting paths went non-owning, the
        // first count alone owned all three.
        assert!(
            owned_bytes < 2 * payload_bytes,
            "counting and cap selection must not materialize every candidate \
             (owned {owned_bytes} bytes for {payload_bytes}-byte payloads)"
        );

        // The chosen-set semantics are visible in the output: the two oldest
        // messages were stripped as units and keep only their prose, the
        // newest kept its prose plus the refusal note, and no raw marker
        // survives anywhere.
        assert!(!result.contains_images);
        for (index, message) in result.messages.iter().enumerate() {
            assert!(
                !message.content.contains("data:image")
                    && !message.content.contains(&payload[..128]),
                "message {index}: the raw marker must not reach provider-visible content"
            );
        }
        assert!(
            result.messages[0].content.contains("message 0"),
            "the trimmed messages keep their text, losing only the image"
        );
        assert!(
            result.messages[2].content.contains("message 2")
                && result.messages[2].content.contains("could not be loaded"),
            "the surviving candidate is refused by the encoded ceiling, not dropped silently"
        );
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

    /// Runs `body` with a fresh [`BASE64_DECODE_CALLS`] counter scoped to it,
    /// and returns `(body's value, base64 decodes observed)`.
    ///
    /// Distinct from [`counting_decodes`]: that counter only observes the
    /// *pixel* decoder, which an oversized data URI never reaches whether it is
    /// rejected before or after `STANDARD.decode`. This one observes the base64
    /// decode itself, which is the allocation the encoded-length ceiling exists
    /// to prevent.
    async fn counting_base64_decodes<F, T>(body: F) -> (T, usize)
    where
        F: std::future::Future<Output = T>,
    {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        BASE64_DECODE_CALLS
            .scope(counter, async move {
                let value = body.await;
                let count = BASE64_DECODE_CALLS
                    .with(|calls| calls.load(std::sync::atomic::Ordering::Relaxed));
                (value, count)
            })
            .await
    }

    /// Runs `body` with a fresh [`CANDIDATE_OWNERSHIP_BYTES`] counter scoped
    /// to it, and returns `(body's value, candidate bytes owned)`.
    ///
    /// Distinct from the decode counters: those observe work the ceilings
    /// exist to prevent; this one observes *ownership* — the parser taking a
    /// marker span into an owned `String`. A production-path run whose
    /// counting passes stay non-owning can only accumulate bytes for the
    /// candidates the cap actually chose.
    async fn counting_candidate_ownership<F, T>(body: F) -> (T, usize)
    where
        F: std::future::Future<Output = T>,
    {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        CANDIDATE_OWNERSHIP_BYTES
            .scope(counter, async move {
                let value = body.await;
                let count = CANDIDATE_OWNERSHIP_BYTES
                    .with(|owned| owned.load(std::sync::atomic::Ordering::Relaxed));
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
            ChatMessage::user(format!("photo [IMAGE:{}]", old_png.display())),
            ChatMessage::user(format!("check this [IMAGE:{}]", new_webp.display())),
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

    #[test]
    fn parse_image_markers_refuses_over_ceiling_marker_without_owning_it() {
        // The parser copies a candidate span (twice, for a line-wrapped marker)
        // before any downstream ceiling can see it, so the span itself has to be
        // bounded here. A loadable over-ceiling marker is replaced by fixed
        // text and never becomes a candidate.
        //
        // Both shapes are covered because they take different paths inside
        // `collapse_wrapped_marker`: the single-line path allocates once, the
        // wrapped path allocates a build buffer *and* a trimmed copy.
        let oversized_payload = "A".repeat(MAX_IMAGE_MARKER_BYTES + 1);

        for (label, payload) in [
            ("single-line", oversized_payload.clone()),
            // Newlines force the wrapped branch, which is the costlier one.
            ("wrapped", format!("{oversized_payload}\n  continued")),
        ] {
            let input = format!("before [IMAGE:data:image/png;base64,{payload}] after");
            let (cleaned, refs) = parse_image_markers(&input);

            assert!(
                refs.is_empty(),
                "{label}: an over-ceiling marker must never become a candidate"
            );
            assert!(
                cleaned.contains(REJECTED_IMAGE_MARKER_NOTE),
                "{label}: an over-ceiling marker must become a bounded note"
            );
            assert!(
                !cleaned.contains("data:image") && !cleaned.contains(&oversized_payload[..128]),
                "{label}: the cleaned output must not retain the raw marker"
            );
        }

        let unterminated = format!("before [IMAGE:data:image/png;base64,{oversized_payload}");
        let (cleaned, refs) = parse_image_markers(&unterminated);
        assert!(refs.is_empty());
        assert!(cleaned.contains(REJECTED_IMAGE_MARKER_NOTE));
        assert!(!cleaned.contains("data:image"));

        // A marker at the ceiling is still accepted, so the guard rejects only
        // what is genuinely past it.
        let at_ceiling = "A".repeat(MAX_IMAGE_MARKER_BYTES - "data:image/png;base64,".len());
        let input = format!("[IMAGE:data:image/png;base64,{at_ceiling}]");
        let (_, refs) = parse_image_markers(&input);
        assert_eq!(
            refs.len(),
            1,
            "a marker exactly at the ceiling must still be extracted"
        );
    }

    #[test]
    fn scan_mode_counts_under_ceiling_markers_without_owning_them() {
        // The over-ceiling test above proves a single oversized span is
        // refused before `collapse_wrapped_marker` runs. This one pins the
        // other half of the ownership boundary: markers *under* the ceiling
        // stay unowned while counting, so several large-but-legal markers
        // cannot allocate through a count — only the later, cap-chosen
        // normalization pass may own them.
        let payload = "A".repeat(30 * 1024 * 1024);
        let input = format!(
            "one [IMAGE:data:image/png;base64,{payload}] two [IMAGE:data:image/png;base64,{payload}]"
        );

        let scanned = parse_image_markers_inner(&input, ParseMode::Scan);
        assert_eq!(scanned.loadable_count, 2, "both markers count as loadable");
        assert_eq!(
            scanned.rejected_count, 0,
            "neither marker is over the ceiling"
        );
        assert!(
            scanned.refs.is_empty(),
            "the scan must not own any candidate body"
        );
        assert!(
            scanned.cleaned.is_empty(),
            "the scan must not build cleaned text"
        );

        // The collecting mode owns exactly the loadable set, and its count
        // agrees with what was collected — counting can never disagree with
        // selection because both read the same classifier.
        let collected = parse_image_markers_inner(&input, ParseMode::RewriteAndCollect);
        assert_eq!(collected.loadable_count, collected.refs.len());
        assert_eq!(collected.refs.len(), 2);

        // Rewriting without collecting (stale-history strips) owns nothing
        // either, while still removing the markers from the text.
        let rewritten = parse_image_markers_inner(&input, ParseMode::RewriteOnly);
        assert_eq!(rewritten.loadable_count, 2);
        assert!(rewritten.refs.is_empty());
        assert!(
            !rewritten.cleaned.contains("data:image"),
            "the rewrite must remove the loadable markers"
        );
    }

    #[test]
    fn marker_span_classification_matches_the_collapsed_reference() {
        // `marker_span_is_loadable` decides from a bounded collapsed prefix so
        // no mode ever owns a span just to classify it. That is sound only
        // while every *legal* reference shape is decided within the prefix
        // limit — the longest is a UNC path with a maximal-length (253-byte)
        // DNS hostname, which needs 257 bytes to find the share delimiter.
        // Pin the equivalence for that boundary and the common shapes.
        let mut shapes: Vec<String> = [
            "/absolute/path.png",
            "http://example.com/a.png",
            "https://example.com/b.webp",
            "data:image/png;base64,iVBORw0KGgo=",
            "C:\\Users\\share\\c.png",
            "D:/data/d.png",
            "not a reference",
            "relative/path.png",
            "   ",
            "",
        ]
        .iter()
        .map(|shape| shape.to_string())
        .collect();
        shapes.push(format!(r"\\{}\share\e.png", "s".repeat(253)));
        shapes.push(format!(r"\\{}\share", "s".repeat(253)));
        shapes.push(format!(r"\\{}\share\f.png", "s".repeat(15)));
        for shape in &shapes {
            assert_eq!(
                marker_span_is_loadable(shape),
                is_loadable_image_reference(&collapse_wrapped_marker(shape)),
                "span classification must match the collapsed reference for {shape:?}"
            );
        }

        // Beyond the prefix limit the classifier is deliberately stricter: a
        // UNC server longer than any legal hostname classifies as prose
        // rather than paying to own the span. Divergence in this direction
        // can never collect a candidate the full check would reject — it
        // only refuses an absurd one — and the equivalence above covers
        // every legal shape.
        let absurd_unc = format!(r"\\{}\share\g.png", "s".repeat(600));
        assert!(!marker_span_is_loadable(&absurd_unc));
        assert!(is_loadable_image_reference(&collapse_wrapped_marker(
            &absurd_unc
        )));

        // A line-wrapped marker classifies through the collapse-equivalent
        // prefix path, not the raw text with the newline still in it.
        let wrapped = "data:image/png;base64,iVBO\n  Rw0KGgo=";
        assert!(marker_span_is_loadable(wrapped));
        assert_eq!(
            marker_span_is_loadable(wrapped),
            is_loadable_image_reference(&collapse_wrapped_marker(wrapped))
        );
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
    async fn stale_native_tool_result_replays_over_ceiling_marker_as_note() {
        // Regression for the stale-replay bypass: an over-ceiling loadable
        // marker produces no candidates, and `stripped_image_marker_text`
        // used to read "no candidates" as "nothing changed", so a stale tool
        // result — which the counting passes deliberately exclude — replayed
        // the raw oversized marker into provider-visible text. The strip must
        // consult the parse state, not infer it from an empty candidate list.
        let payload = "A".repeat(MAX_IMAGE_MARKER_BYTES + 1);
        let native_tool_content = serde_json::json!({
            "tool_call_id": "tc1",
            "content": format!("screenshot [IMAGE:data:image/png;base64,{payload}]"),
        })
        .to_string();

        // The later plain user turn makes the tool result stale and leaves
        // the history with no counted image, so preparation takes the
        // no-image fast path — exactly the route that used to forward the
        // raw marker.
        let messages = vec![
            ChatMessage::tool(native_tool_content),
            ChatMessage::user("What did you see?".to_string()),
        ];

        let ((result, base64_decodes), pixel_decodes) = counting_base64_decodes(async {
            counting_decodes(async {
                prepare_messages_for_provider(&messages, &MultimodalConfig::default())
                    .await
                    .expect("preparation must not hard-fail on a refused marker")
            })
            .await
        })
        .await;

        let value: serde_json::Value = serde_json::from_str(&result.messages[0].content)
            .expect("stale native tool result should remain valid JSON");
        let inner = value
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content should remain a JSON string");
        assert!(
            inner.contains("screenshot"),
            "surrounding prose must survive"
        );
        assert!(
            inner.contains(REJECTED_IMAGE_MARKER_NOTE),
            "the refusal must replace the raw marker"
        );
        assert!(
            !inner.contains("data:image") && !inner.contains(&payload[..128]),
            "the raw marker must not survive the stale replay"
        );
        assert_eq!(
            base64_decodes, 0,
            "rejection must happen before base64 decoding"
        );
        assert_eq!(
            pixel_decodes, 0,
            "rejection must happen before any pixel decode"
        );
    }

    #[tokio::test]
    async fn stale_prompt_tool_result_replays_over_ceiling_marker_in_full_preparation() {
        // The in-loop replay branch strips stale carriers on the *full*
        // preparation path too — the second route to the same helper. A
        // later user image keeps that path live while the stale prompt
        // carrier holds the over-ceiling marker.
        let temp = tempfile::tempdir().unwrap();
        let fresh_path = temp.path().join("fresh-user-image.png");
        std::fs::write(&fresh_path, valid_png()).unwrap();

        let payload = "A".repeat(MAX_IMAGE_MARKER_BYTES + 1);
        let messages = vec![
            ChatMessage::user(format!(
                "[Tool results]\nGenerated [IMAGE:data:image/png;base64,{payload}]"
            )),
            ChatMessage::user(format!(
                "and here is a fresh one [IMAGE:{}]",
                fresh_path.display()
            )),
        ];

        let prepared = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect("preparation must not hard-fail on a refused marker");

        assert!(
            prepared.contains_images,
            "the fresh valid image must survive"
        );
        let stale = &prepared.messages[0].content;
        assert!(
            stale.contains("[Tool results]") && stale.contains("Generated"),
            "the stale carrier keeps its prose"
        );
        assert!(
            stale.contains(REJECTED_IMAGE_MARKER_NOTE),
            "the refusal must replace the raw marker"
        );
        assert!(
            !stale.contains("data:image") && !stale.contains(&payload[..128]),
            "the raw marker must not survive the stale replay"
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
