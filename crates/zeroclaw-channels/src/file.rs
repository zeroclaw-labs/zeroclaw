//! Shared file/image download utilities for channel implementations.

use base64::Engine;
use std::time::Duration;

/// Maximum image size we will download and inline (5 MiB).
pub const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

/// Image MIME types we support for inline base64 encoding.
pub const SUPPORTED_IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

/// Map an image MIME type to its conventional file extension.
pub fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "bin",
    }
}

/// Detect image MIME type from magic bytes, falling back to Content-Type header.
pub fn detect_image_mime(content_type: Option<&str>, bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        return Some("image/png".to_string());
    }
    if bytes.len() >= 3 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg".to_string());
    }
    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some("image/gif".to_string());
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp".to_string());
    }
    if bytes.len() >= 2 && bytes.starts_with(b"BM") {
        return Some("image/bmp".to_string());
    }
    content_type
        .and_then(|ct| ct.split(';').next())
        .map(|ct| ct.trim().to_lowercase())
        .filter(|ct| ct.starts_with("image/"))
}

/// Build a reqwest HTTP client with reasonable timeouts.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Download image from URL and return as `[IMAGE:data:...]` marker.
/// Returns `None` if download fails, image is too large, or unsupported MIME type.
pub async fn download_image_as_base64(url: &str) -> Option<String> {
    let client = build_http_client();

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("download_image_as_base64: request failed: url={}, err={}", url, e);
            return None;
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body_snippet = resp.text().await.unwrap_or_default();
        let body_snippet = body_snippet.chars().take(256).collect::<String>();
        tracing::warn!(
            "download_image_as_base64: HTTP error: url={}, status={}, body={}",
            url, status, body_snippet
        );
        return None;
    }

    if let Some(cl) = resp.content_length() && cl > IMAGE_MAX_BYTES as u64 {
        tracing::warn!(
            "download_image_as_base64: image too large: url={}, size={} bytes, limit={}",
            url, cl, IMAGE_MAX_BYTES
        );
        return None;
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("download_image_as_base64: body read failed: url={}, err={}", url, e);
            return None;
        }
    };

    if bytes.is_empty() || bytes.len() > IMAGE_MAX_BYTES {
        tracing::warn!(
            "download_image_as_base64: body empty or too large: url={}, size={} bytes",
            url, bytes.len()
        );
        return None;
    }

    let mime = match detect_image_mime(content_type.as_deref(), &bytes) {
        Some(m) => m,
        None => {
            tracing::warn!("download_image_as_base64: unsupported image MIME for url={}: {:?}", url, content_type);
            return None;
        }
    };

    if !SUPPORTED_IMAGE_MIMES.contains(&mime.as_str()) {
        tracing::warn!(
            "download_image_as_base64: unsupported MIME type: url={}, mime={}",
            url, mime
        );
        return None;
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("[IMAGE:data:{mime};base64,{encoded}]"))
}
