//! Materialize embedded `resource.blob` payloads into the session workspace.
//! Store-agnostic: no RPC `SessionStore` / `file/attach`. Used by ACP inbound
//! prompt intake; the store-agnostic helper is reusable by other protocol
//! adapters.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Per-file decoded size limit for embedded blobs (matches RPC attach / ACP).
pub const MAX_EMBEDDED_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Result of writing an embedded resource into the session workspace.
#[derive(Debug)]
pub struct MaterializedResource {
    pub abs_path: PathBuf,
    pub marker: String,
    pub mime_type: String,
    pub filename: String,
}

/// Error while decoding or persisting an embedded blob.
#[derive(Debug)]
pub struct EmbeddedResourceError(pub String);

impl std::fmt::Display for EmbeddedResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EmbeddedResourceError {}

/// Decode `blob_b64`, enforce size limits, write under `{workspace}/uploads/`,
/// and return a prompt marker (`[Document: …]` or `[IMAGE:…]`).
pub fn materialize_resource_blob(
    workspace_dir: &Path,
    uri: Option<&str>,
    mime_type: Option<&str>,
    blob_b64: &str,
) -> Result<MaterializedResource, EmbeddedResourceError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.trim())
        .map_err(|e| EmbeddedResourceError(format!("Invalid base64: {e}")))?;

    if bytes.len() as u64 > MAX_EMBEDDED_FILE_BYTES {
        return Err(EmbeddedResourceError(format!(
            "Embedded resource exceeds {} MB limit ({} bytes)",
            MAX_EMBEDDED_FILE_BYTES / (1024 * 1024),
            bytes.len()
        )));
    }

    let filename = sanitize_filename(&filename_from_uri(uri));
    let mime = mime_type
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| mime_from_filename(&filename));

    let hex = format!("{:x}", Sha256::digest(&bytes));
    let ext = Path::new(&filename)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let storage_name = if ext.is_empty() {
        hex[..16].to_string()
    } else {
        format!("{}.{ext}", &hex[..16])
    };

    let upload_dir = workspace_dir.join("uploads");
    std::fs::create_dir_all(&upload_dir)
        .map_err(|e| EmbeddedResourceError(format!("Cannot create upload dir: {e}")))?;
    let dest = upload_dir.join(&storage_name);

    let needs_write = match std::fs::metadata(&dest) {
        Ok(meta) => meta.len() != bytes.len() as u64,
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&dest, &bytes)
            .map_err(|e| EmbeddedResourceError(format!("Cannot write upload: {e}")))?;
    }

    let abs_path = std::fs::canonicalize(&dest).unwrap_or(dest);
    let abs_display = strip_windows_verbatim_prefix(&abs_path.to_string_lossy()).into_owned();
    let marker = if mime.starts_with("image/") {
        format!("[IMAGE:{abs_display}]")
    } else {
        format!("[Document: {filename}] {abs_display}")
    };

    Ok(MaterializedResource {
        abs_path,
        marker,
        mime_type: mime,
        filename,
    })
}

fn filename_from_uri(uri: Option<&str>) -> String {
    let Some(uri) = uri.map(str::trim).filter(|s| !s.is_empty()) else {
        return "upload.bin".to_string();
    };
    let without_scheme = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("attachment://"))
        .unwrap_or(uri);
    let name = without_scheme
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_scheme)
        .trim();
    if name.is_empty() || name == "." || name == ".." {
        "upload.bin".to_string()
    } else {
        name.to_string()
    }
}

fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let sanitized = sanitized.replace("..", "_");
    if sanitized.is_empty() {
        "upload.bin".to_string()
    } else {
        sanitized
    }
}

fn mime_from_filename(filename: &str) -> String {
    match Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png".into(),
        Some("jpg" | "jpeg") => "image/jpeg".into(),
        Some("gif") => "image/gif".into(),
        Some("webp") => "image/webp".into(),
        Some("pdf") => "application/pdf".into(),
        Some("docx") => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into()
        }
        Some("doc") => "application/msword".into(),
        Some("txt") => "text/plain".into(),
        Some("md") => "text/markdown".into(),
        Some("json") => "application/json".into(),
        _ => "application/octet-stream".into(),
    }
}

fn strip_windows_verbatim_prefix(path: &str) -> std::borrow::Cow<'_, str> {
    path.strip_prefix(r"\\?\")
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| std::borrow::Cow::Borrowed(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_blob_under_uploads_and_returns_document_marker() {
        let dir = tempdir().unwrap();
        let bytes = b"hello docx";
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
        let out = materialize_resource_blob(
            dir.path(),
            Some("file:///x/report.docx"),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            &b64,
        )
        .unwrap();
        assert!(out.abs_path.exists());
        assert!(out.marker.contains("[Document: report.docx]"));
        assert!(
            out.marker.contains(out.abs_path.to_string_lossy().as_ref())
                || out.marker.contains(
                    strip_windows_verbatim_prefix(&out.abs_path.to_string_lossy()).as_ref()
                )
        );
        assert_eq!(std::fs::read(&out.abs_path).unwrap(), bytes);
    }

    #[test]
    fn image_mime_uses_image_marker() {
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"img");
        let out =
            materialize_resource_blob(dir.path(), Some("file:///a.png"), Some("image/png"), &b64)
                .unwrap();
        assert!(out.marker.starts_with("[IMAGE:"));
    }

    #[test]
    fn rejects_invalid_base64() {
        let dir = tempdir().unwrap();
        let err = materialize_resource_blob(dir.path(), None, None, "%%%").unwrap_err();
        assert!(err.to_string().to_lowercase().contains("base64"));
    }

    #[test]
    fn rejects_oversized_blob() {
        let dir = tempdir().unwrap();
        let big = vec![0u8; (MAX_EMBEDDED_FILE_BYTES as usize) + 1];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &big);
        let err =
            materialize_resource_blob(dir.path(), Some("file:///big.bin"), None, &b64).unwrap_err();
        assert!(err.to_string().contains("MB") || err.to_string().contains("limit"));
    }
}
