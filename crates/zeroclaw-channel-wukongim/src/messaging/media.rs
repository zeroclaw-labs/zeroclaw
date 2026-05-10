// src/messaging/media.rs
use base64::Engine;
use std::time::Duration;

const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

const SUPPORTED_IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

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

pub async fn download_image_as_base64(url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("wukongim media: request failed: url={url}, err={e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!("wukongim media: HTTP {}: {url}", resp.status());
        return None;
    }
    if let Some(cl) = resp.content_length()
        && cl > IMAGE_MAX_BYTES as u64
    {
        tracing::warn!("wukongim media: image too large ({cl} bytes): {url}");
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
            tracing::warn!("wukongim media: body read failed: {url}, {e}");
            return None;
        }
    };
    if bytes.is_empty() || bytes.len() > IMAGE_MAX_BYTES {
        return None;
    }

    let mime = match detect_image_mime(content_type.as_deref(), &bytes) {
        Some(m) if SUPPORTED_IMAGE_MIMES.contains(&m.as_str()) => m,
        other => {
            tracing::warn!("wukongim media: unsupported MIME {other:?}: {url}");
            return None;
        }
    };

    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("[IMAGE:data:{mime};base64,{encoded}]"))
}

pub fn extract_markdown_images(text: &str) -> Vec<(String, String)> {
    let mut images = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        let after = &rest[start + 2..];
        if let Some(cb) = after.find(']') {
            let alt = after[..cb].to_string();
            let tail = &after[cb + 1..];
            if let Some(inner) = tail.strip_prefix('(')
                && let Some(pe) = inner.find(')')
            {
                images.push((alt, inner[..pe].to_string()));
                rest = &tail[pe + 1..];
                continue;
            }
        }
        break;
    }
    images
}

pub async fn process_markdown_with_images(text: &str) -> String {
    let mut result = text.to_string();
    for (alt, url) in extract_markdown_images(text) {
        if let Some(marker) = download_image_as_base64(&url).await {
            result = result.replace(
                &format!("![{}]({})", alt, url),
                &format!("![{}]({})", alt, marker),
            );
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_no_images_from_plain_text() {
        assert!(extract_markdown_images("Hello world").is_empty());
    }

    #[test]
    fn extract_single_image() {
        let imgs = extract_markdown_images("![logo](https://example.com/logo.png)");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].0, "logo");
        assert_eq!(imgs[0].1, "https://example.com/logo.png");
    }

    #[test]
    fn extract_multiple_images() {
        let text = "![a](https://a.com/a.png) text ![b](https://b.com/b.jpg)";
        let imgs = extract_markdown_images(text);
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].1, "https://a.com/a.png");
        assert_eq!(imgs[1].1, "https://b.com/b.jpg");
    }

    #[test]
    fn detect_png_by_magic_bytes() {
        let png: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0];
        assert_eq!(detect_image_mime(None, png).as_deref(), Some("image/png"));
    }

    #[test]
    fn detect_jpeg_by_magic_bytes() {
        let jpeg: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0];
        assert_eq!(detect_image_mime(None, jpeg).as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn detect_mime_falls_back_to_content_type() {
        assert_eq!(
            detect_image_mime(Some("image/webp; charset=utf-8"), &[0u8; 4]).as_deref(),
            Some("image/webp")
        );
    }

    #[test]
    fn detect_non_image_returns_none() {
        assert!(detect_image_mime(Some("application/json"), &[0u8; 4]).is_none());
    }
}
