use crate::config::{MultimodalConfig, build_runtime_proxy_client_with_timeouts};
use crate::memory::media_cache::{self, MediaCache};
use crate::providers::ChatMessage;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Client;
use std::path::Path;

const IMAGE_MARKER_PREFIX: &str = "[IMAGE:";
const ALLOWED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
];

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
        let candidate = content[marker_start..end].trim();

        if candidate.is_empty() {
            cleaned.push_str(&content[start..=end]);
        } else {
            refs.push(candidate.to_string());
        }

        cursor = end + 1;
    }

    if cursor < content.len() {
        cleaned.push_str(&content[cursor..]);
    }

    (cleaned.trim().to_string(), refs)
}

pub fn count_image_markers(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| parse_image_markers(&m.content).1.len())
        .sum()
}

pub fn contains_image_markers(messages: &[ChatMessage]) -> bool {
    count_image_markers(messages) > 0
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

pub async fn prepare_messages_for_provider(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
) -> anyhow::Result<PreparedMessages> {
    let (max_images, max_image_size_mb) = config.effective_limits();
    let max_bytes = max_image_size_mb.saturating_mul(1024 * 1024);

    let total_images = count_image_markers(messages);

    if total_images == 0 {
        return Ok(PreparedMessages {
            messages: messages.to_vec(),
            contains_images: false,
        });
    }

    // When image count exceeds the limit, strip markers from oldest messages
    // first so that the most recent (most relevant) images survive. This
    // prevents conversations from becoming permanently stuck once the
    // cumulative image count crosses the threshold.
    let trimmed = if total_images > max_images {
        trim_old_images(messages, max_images)
    } else {
        messages.to_vec()
    };

    let remote_client = build_runtime_proxy_client_with_timeouts("provider.ollama", 30, 10);

    let mut normalized_messages = Vec::with_capacity(trimmed.len());
    let mut has_successful_images = false;
    for message in &trimmed {
        if message.role != "user" {
            normalized_messages.push(message.clone());
            continue;
        }

        let (cleaned_text, refs) = parse_image_markers(&message.content);
        if refs.is_empty() {
            normalized_messages.push(message.clone());
            continue;
        }

        let mut normalized_refs = Vec::with_capacity(refs.len());
        for reference in refs {
            let data_uri =
                normalize_image_reference(&reference, config, max_bytes, &remote_client).await?;
            normalized_refs.push(data_uri);
        }

        let content = compose_multimodal_message(&cleaned_text, &normalized_refs);
        normalized_messages.push(ChatMessage {
            role: message.role.clone(),
            content,
        });
    }

    Ok(PreparedMessages {
        messages: normalized_messages,
        contains_images: true,
    })
}

/// Prepare messages for the provider, restoring any cached images first.
///
/// This is the cache-aware entry point: any `[CACHED_IMAGE:{hash}]` markers
/// are resolved back to `[IMAGE:data:...]` before standard preparation runs.
pub async fn prepare_messages_with_cache(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
    media_cache: Option<&MediaCache>,
) -> anyhow::Result<PreparedMessages> {
    let resolved: Vec<ChatMessage> = if let Some(cache) = media_cache {
        messages
            .iter()
            .map(|m| {
                if m.content.contains(media_cache::CACHED_IMAGE_MARKER_PREFIX) {
                    ChatMessage {
                        role: m.role.clone(),
                        content: media_cache::restore_cached_images(&m.content, cache),
                    }
                } else {
                    m.clone()
                }
            })
            .collect()
    } else {
        messages.to_vec()
    };

    prepare_messages_for_provider(&resolved, config).await
}

/// Strip image markers from older messages (oldest first) until total image
/// count is within `max_images`. Keeps the text content of each message.
fn trim_old_images(messages: &[ChatMessage], max_images: usize) -> Vec<ChatMessage> {
    // Find which messages (by index) contain images, oldest first.
    let image_positions: Vec<(usize, usize)> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
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
                m.clone()
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

async fn normalize_image_reference(
    source: &str,
    config: &MultimodalConfig,
    max_bytes: usize,
    remote_client: &Client,
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

    normalize_local_image(source, max_bytes).await
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

fn detect_mime(
    path: Option<&Path>,
    bytes: &[u8],
    header_content_type: Option<&str>,
) -> Option<String> {
    if let Some(header_mime) = header_content_type.and_then(normalize_content_type) {
        return Some(header_mime);
    }

    if let Some(path) = path {
        if let Some(ext) = path.extension().and_then(|value| value.to_str()) {
            if let Some(mime) = mime_from_extension(ext) {
                return Some(mime.to_string());
            }
        }
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
    async fn prepare_messages_trims_excess_images_from_older_messages() {
        // 3 messages, each with 1 image — max is 2.
        // The oldest message's image should be stripped.
        let messages = vec![
            ChatMessage::user("[IMAGE:/tmp/old.png]\nOld caption".to_string()),
            ChatMessage::user("[IMAGE:/tmp/mid.png]\nMid caption".to_string()),
            ChatMessage::user("[IMAGE:/tmp/new.png]\nNew caption".to_string()),
        ];

        // Should not error — instead trims oldest.
        // (Will error on normalize_image_reference for the surviving images
        //  since /tmp/mid.png and /tmp/new.png don't exist, but the trimming
        //  itself should succeed.)
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
            ChatMessage::user(format!("[IMAGE:{}]\nOld", paths[0].display())),
            ChatMessage::user(format!("[IMAGE:{}]\nMid", paths[1].display())),
            ChatMessage::user(format!("[IMAGE:{}]\nNew", paths[2].display())),
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
    async fn prepare_messages_rejects_remote_url_when_disabled() {
        let messages = vec![ChatMessage::user(
            "Look [IMAGE:https://example.com/img.png]".to_string(),
        )];

        let error = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect_err("should reject remote image URL when fetch is disabled");

        assert!(
            error
                .to_string()
                .contains("multimodal remote image fetch is disabled")
        );
    }

    #[tokio::test]
    async fn prepare_messages_rejects_oversized_local_image() {
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

        let error = prepare_messages_for_provider(&messages, &config)
            .await
            .expect_err("should reject oversized local image");

        assert!(
            error
                .to_string()
                .contains("multimodal image size limit exceeded")
        );
    }

    #[test]
    fn extract_ollama_image_payload_supports_data_uris() {
        let payload = extract_ollama_image_payload("data:image/png;base64,abcd==")
            .expect("payload should be extracted");
        assert_eq!(payload, "abcd==");
    }

    /// Stripping `[IMAGE:]` markers from history messages leaves only the text
    /// portion, which is the behaviour needed for non-vision providers (#3674).
    #[test]
    fn parse_image_markers_strips_markers_leaving_caption() {
        let input = "[IMAGE:/tmp/photo.jpg]\n\nDescribe this screenshot";
        let (cleaned, refs) = parse_image_markers(input);
        assert_eq!(cleaned, "Describe this screenshot");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0], "/tmp/photo.jpg");
    }

    /// An image-only message (no caption) should produce an empty string after
    /// marker stripping, so callers can drop it from history.
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
