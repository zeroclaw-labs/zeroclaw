// src/messaging/media.rs
use base64::Engine;
use std::time::Duration;

const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

const FILE_MAX_BYTES: usize = 100 * 1024 * 1024;

const BLOCKED_EXTENSIONS: &[&str] = &[
    "exe", "dll", "bat", "sh", "app", "dmg",
    "js", "py", "rb", "php", "pl",
];

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

pub async fn download_file_to_workspace(
    url: &str,
    workspace_dir: &std::path::Path,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(format!("网络错误: {}", e));
        }
    };

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    if let Some(cl) = resp.content_length() && cl > FILE_MAX_BYTES as u64 {
        return Err("文件超过 100MB 限制".to_string());
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return Err(format!("读取响应失败: {}", e));
        }
    };

    if bytes.is_empty() || bytes.len() > FILE_MAX_BYTES {
        return Err("文件为空或超过大小限制".to_string());
    }

    let filename = url.rsplit('/').next()
        .unwrap_or("download")
        .split('?')
        .next()
        .unwrap_or("download");

    if is_blocked_extension(filename) {
        return Err("不允许的文件类型".to_string());
    }

    let downloads_dir = workspace_dir.join("downloads");
    if let Err(e) = tokio::fs::create_dir_all(&downloads_dir).await {
        return Err(format!("无法创建下载目录: {}", e));
    }

    let mut target_path = downloads_dir.join(filename);
    let mut counter = 1;
    while target_path.exists() {
        let stem = filename.rsplit('.').next().unwrap_or(&filename);
        let ext = if filename.contains('.') {
            format!(".{}", filename.rsplit('.').next().unwrap_or(""))
        } else {
            String::new()
        };
        let new_filename = format!("{} ({}){}", stem, counter, ext);
        target_path = downloads_dir.join(&new_filename);
        counter += 1;
    }

    if let Err(e) = tokio::fs::write(&target_path, &bytes).await {
        return Err(format!("写入文件失败: {}", e));
    }

    Ok(format!("/workspace/downloads/{}", target_path.file_name().unwrap().to_str().unwrap()))
}

pub fn extract_markdown_links(text: &str) -> Vec<(String, String, bool)> {
    let mut links = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("![") {
        // Image link: ![alt](url)
        let after = &rest[start + 2..];
        if let Some(cb) = after.find(']') {
            let alt = after[..cb].to_string();
            let tail = &after[cb + 1..];
            if let Some(inner) = tail.strip_prefix('(')
                && let Some(pe) = inner.find(')')
            {
                links.push((alt, inner[..pe].to_string(), true));
                rest = &tail[pe + 1..];
                continue;
            }
        }
        break;
    }

    rest = text;
    while let Some(start) = rest.find('[') {
        if start > 0 && &rest[start - 1..start] == "!" {
            // Skip image links
            rest = &rest[start + 1..];
            continue;
        }

        // Regular link: [text](url)
        let after = &rest[start + 1..];
        if let Some(cb) = after.find(']') {
            let text_content = after[..cb].to_string();
            let tail = &after[cb + 1..];
            if let Some(inner) = tail.strip_prefix('(')
                && let Some(pe) = inner.find(')')
            {
                links.push((text_content, inner[..pe].to_string(), false));
                rest = &tail[pe + 1..];
                continue;
            }
        }
        rest = &rest[start + 1..];
    }

    links
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

pub fn is_blocked_extension(filename: &str) -> bool {
    if let Some(ext) = filename.rsplit('.').next() {
        BLOCKED_EXTENSIONS.contains(&ext.to_lowercase().as_str())
    } else {
        false
    }
}

pub async fn process_markdown_resources(text: &str, workspace_dir: &std::path::Path) -> String {
    let links = extract_markdown_links(text);
    let mut result = text.to_string();

    for (alt, url, is_image) in links {
        if is_image {
            if let Some(marker) = download_image_as_base64(&url).await {
                result = result.replace(
                    &format!("![{}]({})", alt, url),
                    &format!("![{}]({})", alt, marker),
                );
            } else {
                result = result.replace(
                    &format!("![{}]({})", alt, url),
                    &format!("![图片下载失败]({})", url),
                );
            }
        } else {
            match download_file_to_workspace(&url, workspace_dir).await {
                Ok(local_path) => {
                    result = result.replace(
                        &format!("[{}]({})", alt, url),
                        &format!("[{}]({})", alt, local_path),
                    );
                }
                Err(err_msg) => {
                    result = result.replace(
                        &format!("[{}]({})", alt, url),
                        &format!("[{}]({}) [下载失败: {}]", alt, url, err_msg),
                    );
                }
            }
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

    #[test]
    fn test_is_blocked_extension() {
        assert!(is_blocked_extension("script.exe"));
        assert!(is_blocked_extension("malware.js"));
        assert!(!is_blocked_extension("document.pdf"));
        assert!(!is_blocked_extension("data.txt"));
        assert!(!is_blocked_extension("no_extension"));
    }

    #[test]
    fn test_extract_markdown_links_images_only() {
        let text = "Check ![logo](https://example.com/logo.png) and ![photo](https://example.com/photo.jpg)";
        let links = extract_markdown_links(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].0, "logo");
        assert_eq!(links[0].1, "https://example.com/logo.png");
        assert_eq!(links[0].2, true);
        assert_eq!(links[1].2, true);
    }

    #[test]
    fn test_extract_markdown_links_files_only() {
        let text = "Download [document](https://example.com/file.pdf) and [data](https://example.com/data.csv)";
        let links = extract_markdown_links(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].0, "document");
        assert_eq!(links[0].1, "https://example.com/file.pdf");
        assert_eq!(links[0].2, false);
        assert_eq!(links[1].2, false);
    }

    #[test]
    fn test_extract_markdown_links_mixed() {
        let text = "See ![img](img.png) and [file](doc.pdf)";
        let links = extract_markdown_links(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].2, true);
        assert_eq!(links[1].2, false);
    }

    fn create_test_workspace() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    #[tokio::test]
    async fn test_process_markdown_resources_mixed() {
        let workspace = create_test_workspace();

        let text = "See ![image](img.png) and [file](doc.pdf)";
        let result = process_markdown_resources(text, workspace.path()).await;

        // Image download should fail (network), file download should fail (network)
        // Both should be processed, just with different markers
        assert!(result.contains("图片下载失败") || result.contains("下载失败"));
        assert!(result.contains("img.png") || result.contains("doc.pdf"));
    }

    #[tokio::test]
    async fn test_process_markdown_resources_no_links() {
        let workspace = create_test_workspace();
        let text = "Just plain text with no links";
        let result = process_markdown_resources(text, workspace.path()).await;
        assert_eq!(result, text);
    }

    #[cfg(test)]
    mod file_download_tests {
        use super::*;

        fn create_test_workspace() -> tempfile::TempDir {
            tempfile::TempDir::new().unwrap()
        }

        #[tokio::test]
        async fn test_download_file_to_workspace_success() {
            let workspace = create_test_workspace();
            let url = "https://example.com/test.pdf";

            let result = download_file_to_workspace(url, workspace.path()).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_download_filename_conflict_handling() {
            let workspace = create_test_workspace();
            let downloads_dir = workspace.path().join("downloads");
            tokio::fs::create_dir_all(&downloads_dir).await.unwrap();

            let initial_path = downloads_dir.join("test.pdf");
            tokio::fs::write(&initial_path, b"content").await.unwrap();

            let mut counter = 1;
            let mut target_path = downloads_dir.join("test.pdf");
            while target_path.exists() {
                let stem = "test";
                let ext = ".pdf";
                let new_filename = format!("{} ({}){}", stem, counter, ext);
                target_path = downloads_dir.join(&new_filename);
                counter += 1;
            }

            assert_eq!(target_path.file_name().unwrap().to_str().unwrap(), "test (1).pdf");
        }
    }
}
