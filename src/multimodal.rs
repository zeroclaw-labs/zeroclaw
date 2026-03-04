use crate::config::{build_runtime_proxy_client_with_timeouts, MultimodalConfig};
use crate::providers::ChatMessage;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Client;
use std::io::Cursor;
use std::path::Path;

const IMAGE_MARKER_PREFIX: &str = "[IMAGE:";
const OPTIMIZED_IMAGE_MAX_DIMENSION: u32 = 512;
const OPTIMIZED_IMAGE_TARGET_BYTES: usize = 256 * 1024;
const REMOTE_FETCH_MULTIMODAL_SERVICE_KEY: &str = "tool.multimodal";
const REMOTE_FETCH_TOOL_SERVICE_KEY: &str = "tool.http_request";
const REMOTE_FETCH_QQ_SERVICE_KEY: &str = "channel.qq";
const REMOTE_FETCH_LEGACY_SERVICE_KEY: &str = "provider.ollama";
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

    #[error("multimodal image size limit exceeded for '{input}': {size_bytes} bytes > {max_bytes} bytes")]
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
    prepare_messages_for_provider_with_provider_hint(messages, config, None).await
}

pub async fn prepare_messages_for_provider_with_provider_hint(
    messages: &[ChatMessage],
    config: &MultimodalConfig,
    provider_hint: Option<&str>,
) -> anyhow::Result<PreparedMessages> {
    let (max_images, max_image_size_mb) = config.effective_limits();
    let max_bytes = max_image_size_mb.saturating_mul(1024 * 1024);

    let found_images = count_image_markers(messages);
    if found_images > max_images {
        return Err(MultimodalError::TooManyImages {
            max_images,
            found: found_images,
        }
        .into());
    }

    if found_images == 0 {
        return Ok(PreparedMessages {
            messages: messages.to_vec(),
            contains_images: false,
        });
    }

    let mut normalized_messages = Vec::with_capacity(messages.len());
    for message in messages {
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
                normalize_image_reference(&reference, config, max_bytes, provider_hint).await?;
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
    provider_hint: Option<&str>,
) -> anyhow::Result<String> {
    if source.starts_with("data:") {
        return normalize_data_uri(source, max_bytes).await;
    }

    if source.starts_with("http://") || source.starts_with("https://") {
        if !config.allow_remote_fetch {
            return Err(MultimodalError::RemoteFetchDisabled {
                input: source.to_string(),
            }
            .into());
        }

        return normalize_remote_image(source, max_bytes, provider_hint).await;
    }

    normalize_local_image(source, max_bytes).await
}

async fn normalize_data_uri(source: &str, max_bytes: usize) -> anyhow::Result<String> {
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

    let (optimized_bytes, optimized_mime) =
        optimize_image_for_prompt(source, decoded, &mime).await?;
    validate_size(source, optimized_bytes.len(), max_bytes)?;

    Ok(format!(
        "data:{optimized_mime};base64,{}",
        STANDARD.encode(optimized_bytes)
    ))
}

async fn normalize_remote_image(
    source: &str,
    max_bytes: usize,
    provider_hint: Option<&str>,
) -> anyhow::Result<String> {
    let service_keys = build_remote_fetch_service_keys(source, provider_hint);
    let mut failures = Vec::new();

    for service_key in service_keys {
        let client = build_runtime_proxy_client_with_timeouts(&service_key, 30, 10);
        match normalize_remote_image_once(source, max_bytes, &client).await {
            Ok(normalized) => return Ok(normalized),
            Err(error) => {
                let reason = error.to_string();
                tracing::debug!(
                    service_key = %service_key,
                    source = %source,
                    "multimodal remote fetch attempt failed: {reason}"
                );
                failures.push(format!("{service_key}: {reason}"));
            }
        }
    }

    Err(MultimodalError::RemoteFetchFailed {
        input: source.to_string(),
        reason: format!(
            "{}; hint: when proxy.scope='services', include one of channel.qq/tool.multimodal/tool.http_request/provider.* as needed",
            failures.join(" | ")
        ),
    }
    .into())
}

async fn normalize_remote_image_once(
    source: &str,
    max_bytes: usize,
    remote_client: &Client,
) -> anyhow::Result<String> {
    let mut request = remote_client
        .get(source)
        .header(reqwest::header::USER_AGENT, "ZeroClaw/1.0");
    if source_looks_like_qq_media(source) {
        request = request.header(reqwest::header::REFERER, "https://qq.com/");
    }

    let response = request
        .send()
        .await
        .map_err(|error| anyhow::anyhow!("error sending request for url ({source}): {error}"))?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
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
        .map_err(|error| anyhow::anyhow!("failed to read response body: {error}"))?;

    validate_size(source, bytes.len(), max_bytes)?;

    let mime = detect_mime(None, bytes.as_ref(), content_type.as_deref()).ok_or_else(|| {
        MultimodalError::UnsupportedMime {
            input: source.to_string(),
            mime: "unknown".to_string(),
        }
    })?;

    validate_mime(source, &mime)?;
    let (optimized_bytes, optimized_mime) =
        optimize_image_for_prompt(source, bytes.to_vec(), &mime).await?;
    validate_size(source, optimized_bytes.len(), max_bytes)?;

    Ok(format!(
        "data:{optimized_mime};base64,{}",
        STANDARD.encode(optimized_bytes)
    ))
}

fn normalize_provider_service_key_hint(provider_hint: Option<&str>) -> Option<String> {
    let raw = provider_hint
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())?
        .split('#')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if raw.is_empty() {
        return None;
    }

    let candidate = if raw.starts_with("provider.") {
        raw
    } else {
        format!("provider.{raw}")
    };

    if !candidate
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-'))
    {
        return None;
    }

    Some(candidate)
}

fn source_looks_like_qq_media(source: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(source) else {
        return false;
    };

    let Some(host) = parsed.host_str() else {
        return false;
    };

    let host = host.to_ascii_lowercase();
    host == "multimedia.nt.qq.com.cn" || host.ends_with(".qq.com.cn") || host.ends_with(".qq.com")
}

fn push_service_key_once(keys: &mut Vec<String>, key: String) {
    if !key.trim().is_empty() && !keys.iter().any(|existing| existing == &key) {
        keys.push(key);
    }
}

fn build_remote_fetch_service_keys(source: &str, provider_hint: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();

    if source_looks_like_qq_media(source) {
        push_service_key_once(&mut keys, REMOTE_FETCH_QQ_SERVICE_KEY.to_string());
    }

    if let Some(provider_service_key) = normalize_provider_service_key_hint(provider_hint) {
        push_service_key_once(&mut keys, provider_service_key);
    }

    push_service_key_once(&mut keys, REMOTE_FETCH_MULTIMODAL_SERVICE_KEY.to_string());
    push_service_key_once(&mut keys, REMOTE_FETCH_TOOL_SERVICE_KEY.to_string());
    push_service_key_once(&mut keys, REMOTE_FETCH_LEGACY_SERVICE_KEY.to_string());
    keys
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
    let (optimized_bytes, optimized_mime) = optimize_image_for_prompt(source, bytes, &mime).await?;
    validate_size(source, optimized_bytes.len(), max_bytes)?;

    Ok(format!(
        "data:{optimized_mime};base64,{}",
        STANDARD.encode(optimized_bytes)
    ))
}

async fn optimize_image_for_prompt(
    source: &str,
    bytes: Vec<u8>,
    mime: &str,
) -> anyhow::Result<(Vec<u8>, String)> {
    validate_mime(source, mime)?;

    let source_owned = source.to_string();
    let mime_owned = mime.to_string();
    tokio::task::spawn_blocking(move || {
        optimize_image_for_prompt_blocking(source_owned, bytes, mime_owned)
    })
    .await
    .map_err(|error| MultimodalError::InvalidMarker {
        input: source.to_string(),
        reason: format!("failed to optimize image payload: {error}"),
    })?
}

fn optimize_image_for_prompt_blocking(
    source: String,
    bytes: Vec<u8>,
    mime: String,
) -> anyhow::Result<(Vec<u8>, String)> {
    let decoded = match image::load_from_memory(&bytes) {
        Ok(decoded) => decoded,
        Err(_) => return Ok((bytes, mime)),
    };

    let resized = if decoded.width() > OPTIMIZED_IMAGE_MAX_DIMENSION
        || decoded.height() > OPTIMIZED_IMAGE_MAX_DIMENSION
    {
        decoded.thumbnail(OPTIMIZED_IMAGE_MAX_DIMENSION, OPTIMIZED_IMAGE_MAX_DIMENSION)
    } else {
        decoded
    };

    let mut best_jpeg = Vec::new();
    for quality in [85_u8, 70_u8, 55_u8, 40_u8] {
        let mut encoded = Vec::new();
        {
            let mut cursor = Cursor::new(&mut encoded);
            let mut encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality);
            encoder
                .encode_image(&resized)
                .map_err(|error| MultimodalError::InvalidMarker {
                    input: source.clone(),
                    reason: format!("failed to encode optimized image: {error}"),
                })?;
        }

        best_jpeg = encoded;
        if best_jpeg.len() <= OPTIMIZED_IMAGE_TARGET_BYTES {
            return Ok((best_jpeg, "image/jpeg".to_string()));
        }
    }

    if best_jpeg.len() < bytes.len() {
        return Ok((best_jpeg, "image/jpeg".to_string()));
    }

    Ok((bytes, mime))
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
    if mime.is_empty() {
        None
    } else {
        Some(mime)
    }
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
    async fn prepare_messages_rejects_too_many_images() {
        let messages = vec![ChatMessage::user(
            "[IMAGE:/tmp/1.png]\n[IMAGE:/tmp/2.png]".to_string(),
        )];

        let config = MultimodalConfig {
            max_images: 1,
            max_image_size_mb: 5,
            allow_remote_fetch: false,
        };

        let error = prepare_messages_for_provider(&messages, &config)
            .await
            .expect_err("should reject image count overflow");

        assert!(error
            .to_string()
            .contains("multimodal image limit exceeded"));
    }

    #[tokio::test]
    async fn prepare_messages_rejects_remote_url_when_disabled() {
        let messages = vec![ChatMessage::user(
            "Look [IMAGE:https://example.com/img.png]".to_string(),
        )];

        let error = prepare_messages_for_provider(&messages, &MultimodalConfig::default())
            .await
            .expect_err("should reject remote image URL when fetch is disabled");

        assert!(error
            .to_string()
            .contains("multimodal remote image fetch is disabled"));
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
        };

        let error = prepare_messages_for_provider(&messages, &config)
            .await
            .expect_err("should reject oversized local image");

        assert!(error
            .to_string()
            .contains("multimodal image size limit exceeded"));
    }

    #[tokio::test]
    async fn normalize_data_uri_downscales_large_images_for_prompt_budget() {
        let mut image = image::RgbImage::new(1800, 1200);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8]);
        }

        let mut png_bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut png_bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let original_size = png_bytes.len();

        let source = format!("data:image/png;base64,{}", STANDARD.encode(&png_bytes));
        let optimized = normalize_data_uri(&source, 5 * 1024 * 1024)
            .await
            .expect("data uri should normalize");
        assert!(optimized.starts_with("data:image/jpeg;base64,"));

        let payload = optimized
            .split_once(',')
            .map(|(_, payload)| payload)
            .expect("optimized data URI payload");
        let optimized_bytes = STANDARD.decode(payload).expect("base64 decode");
        assert!(
            optimized_bytes.len() < original_size,
            "optimized bytes should be smaller than original PNG payload"
        );

        let optimized_image = image::load_from_memory(&optimized_bytes).expect("decode optimized");
        assert!(optimized_image.width() <= OPTIMIZED_IMAGE_MAX_DIMENSION);
        assert!(optimized_image.height() <= OPTIMIZED_IMAGE_MAX_DIMENSION);
    }

    #[test]
    fn normalize_provider_service_key_hint_builds_provider_prefix() {
        assert_eq!(
            normalize_provider_service_key_hint(Some("openai")),
            Some("provider.openai".to_string())
        );
        assert_eq!(
            normalize_provider_service_key_hint(Some("provider.gemini")),
            Some("provider.gemini".to_string())
        );
        assert_eq!(normalize_provider_service_key_hint(Some("   ")), None);
        assert_eq!(normalize_provider_service_key_hint(None), None);
        assert_eq!(
            normalize_provider_service_key_hint(Some("openai#fast-route")),
            Some("provider.openai".to_string())
        );
        assert_eq!(
            normalize_provider_service_key_hint(Some("provider.gemini#img")),
            Some("provider.gemini".to_string())
        );
        assert_eq!(
            normalize_provider_service_key_hint(Some("custom:https://api.example.com/v1")),
            None
        );
    }

    #[test]
    fn build_remote_fetch_service_keys_prefers_qq_channel_for_qq_media_hosts() {
        let keys = build_remote_fetch_service_keys(
            "https://multimedia.nt.qq.com.cn/download?appid=1406",
            Some("openai"),
        );
        assert_eq!(
            keys,
            vec![
                "channel.qq".to_string(),
                "provider.openai".to_string(),
                "tool.multimodal".to_string(),
                "tool.http_request".to_string(),
                "provider.ollama".to_string(),
            ]
        );
    }

    #[test]
    fn build_remote_fetch_service_keys_deduplicates_service_candidates() {
        let keys = build_remote_fetch_service_keys("https://example.com/a.png", Some("ollama"));
        assert_eq!(
            keys,
            vec![
                "provider.ollama".to_string(),
                "tool.multimodal".to_string(),
                "tool.http_request".to_string(),
            ]
        );
    }

    #[test]
    fn extract_ollama_image_payload_supports_data_uris() {
        let payload = extract_ollama_image_payload("data:image/png;base64,abcd==")
            .expect("payload should be extracted");
        assert_eq!(payload, "abcd==");
    }
}
