//! Materialize embedded `resource.blob` payloads into the session workspace.
//! Store-agnostic: no RPC `SessionStore` / `file/attach`. Shared by ACP inbound
//! and MCP tools/call postprocessing.

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

/// `<sha16>` (or `<sha16>.<ext>`) given the full lowercase hex SHA-256 digest.
fn hash16_name(hex: &str, ext: &str) -> String {
    if ext.is_empty() {
        hex[..16].to_string()
    } else {
        format!("{}.{ext}", &hex[..16])
    }
}

/// Content-addressed identity `<sha16>` / `<sha16>.<ext>` derived from raw bytes
/// (first 16 hex chars of their SHA-256). Shared by blob materialization and
/// outbound delivery URIs so both use the same opaque, URI-safe, collision-
/// resistant name — the identity depends on content, never on a caller-supplied
/// filename, so same-name files never collide and reserved characters can't leak.
pub fn content_hash_name(bytes: &[u8], ext: &str) -> String {
    hash16_name(&format!("{:x}", Sha256::digest(bytes)), ext)
}

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
    let storage_name = hash16_name(&hex, &ext);

    // Resolve the workspace to a symlink-free base so containment checks below are
    // meaningful. The blob producer (MCP server / ACP client) controls the bytes and
    // therefore the content-hash filename, so it can pre-plant a symlink at the
    // destination; every write path here must be no-follow and stay inside `ws_root`.
    let ws_root = std::fs::canonicalize(workspace_dir)
        .map_err(|e| EmbeddedResourceError(format!("Cannot resolve workspace dir: {e}")))?;

    let upload_dir = ws_root.join("uploads");
    // A symlinked `uploads/` would redirect every write outside the workspace.
    if let Ok(meta) = std::fs::symlink_metadata(&upload_dir)
        && meta.file_type().is_symlink()
    {
        return Err(EmbeddedResourceError(
            "uploads path is a symlink; refusing to materialize blob".into(),
        ));
    }
    std::fs::create_dir_all(&upload_dir)
        .map_err(|e| EmbeddedResourceError(format!("Cannot create upload dir: {e}")))?;
    // The real uploads dir must live inside the workspace (guards a symlink planted
    // on an intermediate component).
    let upload_real = std::fs::canonicalize(&upload_dir)
        .map_err(|e| EmbeddedResourceError(format!("Cannot resolve upload dir: {e}")))?;
    if !upload_real.starts_with(&ws_root) {
        return Err(EmbeddedResourceError(
            "upload dir resolves outside the workspace; refusing to materialize blob".into(),
        ));
    }

    let dest = upload_real.join(&storage_name);
    // Never follow a symlink sitting at the destination. Drop it; the atomic rename
    // below installs a fresh regular file in its place.
    match std::fs::symlink_metadata(&dest) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::remove_file(&dest).map_err(|e| {
                EmbeddedResourceError(format!("Cannot clear symlink at upload dest: {e}"))
            })?;
        }
        _ => {}
    }

    // Content-addressed dedup, but verify content rather than trusting a length match:
    // an attacker-substituted file of equal length must not be handed back.
    let needs_write = match std::fs::symlink_metadata(&dest) {
        Ok(meta) if meta.file_type().is_file() && meta.len() == bytes.len() as u64 => {
            match std::fs::read(&dest) {
                Ok(existing) => existing != bytes,
                Err(_) => true,
            }
        }
        _ => true,
    };
    if needs_write {
        write_blob_atomic(&upload_real, &dest, &hex, &bytes)?;
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

/// Write `bytes` to `dest` atomically without ever following a symlink at `dest`.
///
/// Writes to a uniquely-named temp file in the same directory (via `create_new`, so
/// a pre-planted symlink at the temp path is refused rather than followed), then
/// renames it over `dest`. `rename` replaces a symlink at `dest` without following it,
/// and the rename is atomic on the same filesystem.
fn write_blob_atomic(
    dir: &Path,
    dest: &Path,
    hex: &str,
    bytes: &[u8],
) -> Result<(), EmbeddedResourceError> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".tmp-{}-{seq}-{}", std::process::id(), &hex[..16]));

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| EmbeddedResourceError(format!("Cannot create upload temp file: {e}")))?;
    let write_result = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| EmbeddedResourceError(format!("Cannot write upload: {e}")));
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    drop(file);

    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(EmbeddedResourceError(format!(
            "Cannot install upload file: {e}"
        )));
    }
    Ok(())
}

/// Whether an MCP tools/call content item is a `resource` with a `blob` field.
pub(crate) fn content_item_has_resource_blob(item: &serde_json::Value) -> bool {
    item.get("type").and_then(|t| t.as_str()) == Some("resource")
        && item
            .get("resource")
            .and_then(|r| r.get("blob"))
            .and_then(|b| b.as_str())
            .is_some()
}

/// Format an MCP `tools/call` result for the model.
///
/// When `content` contains any `type: "resource"` item with `blob`, materialize
/// each blob under `{workspace}/uploads/` and return the full result as JSON with
/// only the binary payloads redacted: a resource `blob` is replaced by a
/// Document/IMAGE `materialized` marker, and image/audio `data` by a concise
/// marker — never raw base64. Every non-binary field (text, `resource_link`,
/// unknown content types, per-item `annotations`, and top-level
/// `structuredContent`/`_meta`/`isError`) is preserved verbatim. Results without a
/// resource blob keep the existing pretty-printed JSON shape.
///
/// Crate-internal: the only caller is [`crate::mcp_tool::McpToolWrapper`]; the
/// serialized `CallToolResult` from `McpRegistry::call_tool` remains the public
/// surface.
pub(crate) fn format_mcp_tool_result_for_model(
    result: &serde_json::Value,
    workspace_dir: &Path,
) -> Result<String, EmbeddedResourceError> {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return Ok(serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()));
    };

    if !content.iter().any(content_item_has_resource_blob) {
        return Ok(serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()));
    }

    // Preserve the entire result and redact ONLY binary payloads. This keeps the
    // machine-readable provenance the model (and downstream tooling) may rely on:
    // structuredContent, _meta, per-item annotations, isError, text, resource_link,
    // and unknown content types all survive; only base64 blob/data are removed.
    let mut redacted = result.clone();
    let Some(items) = redacted.get_mut("content").and_then(|c| c.as_array_mut()) else {
        return Ok(serde_json::to_string_pretty(&redacted).unwrap_or_else(|_| redacted.to_string()));
    };
    for item in items.iter_mut() {
        let typ = item
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        match typ.as_str() {
            "resource" => {
                let Some(blob) = item
                    .get("resource")
                    .and_then(|r| r.get("blob"))
                    .and_then(|b| b.as_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                let uri = item
                    .get("resource")
                    .and_then(|r| r.get("uri"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let mime = item
                    .get("resource")
                    .and_then(|r| r.get("mimeType"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // Degrade per-item: one malformed/oversized blob must not fail the
                // whole result or leak base64.
                let marker = match materialize_resource_blob(
                    workspace_dir,
                    uri.as_deref(),
                    mime.as_deref(),
                    &blob,
                ) {
                    Ok(materialized) => materialized.marker,
                    Err(e) => format!("[attachment unavailable: {e}]"),
                };
                if let Some(res) = item.get_mut("resource").and_then(|r| r.as_object_mut()) {
                    res.remove("blob");
                    res.insert(
                        "materialized".to_string(),
                        serde_json::Value::String(marker),
                    );
                }
            }
            "image" | "audio" => {
                let mime = item
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .unwrap_or("application/octet-stream")
                    .to_string();
                if let Some(obj) = item.as_object_mut()
                    && obj.remove("data").is_some()
                {
                    obj.insert(
                        "materialized".to_string(),
                        serde_json::Value::String(format!("[{typ} attachment: {mime}]")),
                    );
                }
            }
            _ => {
                // text, resource_link and unknown content types carry through verbatim.
            }
        }
    }

    Ok(serde_json::to_string_pretty(&redacted).unwrap_or_else(|_| redacted.to_string()))
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
    use serde_json::json;
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

    // Security: a pre-planted symlink at the destination must never be followed.
    // The MCP/ACP producer can predict the content-hash filename, so it can plant a
    // symlink pointing outside the workspace before the blob is written. Writing
    // through it would clobber an arbitrary file.
    #[cfg(unix)]
    #[test]
    fn symlink_at_dest_is_not_followed_on_write() {
        let ws = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret");
        std::fs::write(&secret, b"TOPSECRET").unwrap();

        // Different length than the legit blob so the length-dedup gate would
        // (in the vulnerable code) decide to write — through the symlink.
        let bytes = b"legit blob payload";
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);

        // First call creates the real destination and tells us its path.
        let first =
            materialize_resource_blob(ws.path(), Some("file:///a.bin"), None, &b64).unwrap();
        let dest = first.abs_path.clone();

        // Attacker replaces it with a symlink to the outside secret.
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink(&secret, &dest).unwrap();

        // Second call with the same bytes resolves to the same dest (now a symlink).
        let _ = materialize_resource_blob(ws.path(), Some("file:///a.bin"), None, &b64);

        // The outside secret must be untouched, and the resolved path must stay
        // inside the workspace.
        assert_eq!(
            std::fs::read(&secret).unwrap(),
            b"TOPSECRET",
            "write followed the symlink and clobbered a file outside the workspace"
        );
        let out = materialize_resource_blob(ws.path(), Some("file:///a.bin"), None, &b64).unwrap();
        assert!(
            out.abs_path.starts_with(ws.path().canonicalize().unwrap()),
            "resolved dest escaped the workspace: {:?}",
            out.abs_path
        );
    }

    // Security: an existing file with the same length but different content must not
    // be trusted. The length-only dedup gate would skip the write and hand the model
    // a marker pointing at substituted content.
    #[test]
    fn same_length_substituted_content_is_not_trusted() {
        let ws = tempdir().unwrap();
        let bytes = b"authentic-content";
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);

        let first =
            materialize_resource_blob(ws.path(), Some("file:///a.bin"), None, &b64).unwrap();
        let dest = first.abs_path.clone();

        // Overwrite with different content of identical length.
        let forged = b"forged!!!!content";
        assert_eq!(forged.len(), bytes.len());
        std::fs::write(&dest, forged).unwrap();

        let out = materialize_resource_blob(ws.path(), Some("file:///a.bin"), None, &b64).unwrap();
        assert_eq!(
            std::fs::read(&out.abs_path).unwrap(),
            bytes,
            "length-only dedup handed back substituted content"
        );
    }

    #[test]
    fn mcp_intake_materializes_blob_and_omits_base64() {
        let dir = tempdir().unwrap();
        let bytes = b"%PDF-1.4 fake";
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
        let result = json!({
            "content": [
                { "type": "text", "text": "Fetched original" },
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///kb/report.pdf",
                        "mimeType": "application/pdf",
                        "blob": b64,
                    }
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("Fetched original"));
        assert!(out.contains("[Document: report.pdf]"));
        assert!(
            !out.contains(&b64),
            "base64 must not reach the model: {out}"
        );
        let uploads = dir.path().join("uploads");
        assert!(uploads.exists());
        let entries: Vec<_> = std::fs::read_dir(&uploads).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().unwrap().path();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn mcp_intake_image_blob_uses_image_marker() {
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"img");
        let result = json!({
            "content": [{
                "type": "resource",
                "resource": {
                    "uri": "file:///a.png",
                    "mimeType": "image/png",
                    "blob": b64,
                }
            }]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("[IMAGE:"));
        assert!(!out.contains(&b64));
    }

    #[test]
    fn mcp_intake_bad_base64_degrades_per_item() {
        // A single malformed blob must degrade to an inline marker, not Err.
        let dir = tempdir().unwrap();
        let result = json!({
            "content": [{
                "type": "resource",
                "resource": {
                    "uri": "file:///x.bin",
                    "blob": "%%%",
                }
            }]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("[attachment unavailable:"));
        assert!(out.to_lowercase().contains("base64"));
    }

    #[test]
    fn mcp_intake_oversized_blob_degrades_per_item() {
        let dir = tempdir().unwrap();
        let big = vec![0u8; (MAX_EMBEDDED_FILE_BYTES as usize) + 1];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &big);
        let result = json!({
            "content": [{
                "type": "resource",
                "resource": {
                    "uri": "file:///big.bin",
                    "blob": b64,
                }
            }]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("[attachment unavailable:"));
        assert!(out.contains("MB") || out.contains("limit"));
    }

    #[test]
    fn mcp_intake_degrades_bad_blob_keeps_sibling_text() {
        // Valid text sibling must survive when a neighbouring blob is malformed.
        let dir = tempdir().unwrap();
        let result = json!({
            "content": [
                { "type": "text", "text": "keep me" },
                {
                    "type": "resource",
                    "resource": { "uri": "file:///x.bin", "blob": "%%%" }
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("keep me"));
        assert!(out.contains("[attachment unavailable:"));
    }

    #[test]
    fn mcp_intake_preserves_resource_link() {
        // resource_link (no blob) is preserved verbatim alongside a redacted blob.
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"doc");
        let result = json!({
            "content": [
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///kb/report.pdf",
                        "mimeType": "application/pdf",
                        "blob": b64,
                    }
                },
                {
                    "type": "resource_link",
                    "uri": "https://example.com/spec",
                    "name": "The Spec",
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("resource_link"));
        assert!(out.contains("The Spec"));
        assert!(out.contains("https://example.com/spec"));
        assert!(!out.contains(&b64));
    }

    #[test]
    fn mcp_intake_image_item_yields_marker_not_base64() {
        // A non-resource `image` block emits a marker, never its base64 data.
        let dir = tempdir().unwrap();
        let doc_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"doc");
        let img_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"this-is-the-raw-image-data",
        );
        let result = json!({
            "content": [
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///kb/report.pdf",
                        "mimeType": "application/pdf",
                        "blob": doc_b64,
                    }
                },
                {
                    "type": "image",
                    "data": img_b64,
                    "mimeType": "image/png",
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("[image attachment: image/png]"));
        assert!(
            !out.contains(&img_b64),
            "raw image base64 must not reach the model: {out}"
        );
    }

    #[test]
    fn mcp_intake_preserves_iserror_and_text() {
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"doc");
        let result = json!({
            "isError": true,
            "content": [
                { "type": "text", "text": "boom" },
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///kb/report.pdf",
                        "mimeType": "application/pdf",
                        "blob": b64,
                    }
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        // The error flag is preserved as structured data, not flattened to prose.
        assert!(out.contains("isError"));
        assert!(out.contains("boom"));
        assert!(!out.contains(&b64));
    }

    #[test]
    fn mcp_intake_preserves_structured_content_meta_and_annotations() {
        // Core MCP#1 property: everything non-binary survives; only the blob is
        // redacted. structuredContent/_meta/annotations must not be silently dropped.
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"%PDF");
        let result = json!({
            "structuredContent": { "rows": 3, "status": "ok" },
            "_meta": { "trace": "abc123" },
            "content": [
                { "type": "text", "text": "summary", "annotations": { "audience": ["user"] } },
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///kb/report.pdf",
                        "mimeType": "application/pdf",
                        "blob": b64,
                    }
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(
            out.contains("structuredContent"),
            "structuredContent dropped: {out}"
        );
        assert!(out.contains("\"rows\""));
        assert!(
            out.contains("_meta") && out.contains("abc123"),
            "_meta dropped: {out}"
        );
        assert!(
            out.contains("annotations") && out.contains("audience"),
            "annotations dropped: {out}"
        );
        assert!(out.contains("summary"));
        // Binary blob is materialized to disk and never leaked as base64.
        assert!(out.contains("[Document: report.pdf]"));
        assert!(!out.contains(&b64), "base64 leaked: {out}");
        assert_eq!(
            std::fs::read_dir(dir.path().join("uploads"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn mcp_intake_without_blob_keeps_pretty_json() {
        let dir = tempdir().unwrap();
        let result = json!({
            "content": [{ "type": "text", "text": "plain" }]
        });
        let out = format_mcp_tool_result_for_model(&result, dir.path()).unwrap();
        assert!(out.contains("\"type\": \"text\"") || out.contains("\"type\":\"text\""));
        assert!(out.contains("plain"));
        assert!(!dir.path().join("uploads").exists());
    }

    #[test]
    fn mcp_intake_gates_on_shape_not_tool_name() {
        // Shape gate: resource+blob is enough; no tool-name checks.
        assert!(content_item_has_resource_blob(&json!({
            "type": "resource",
            "resource": { "uri": "u", "blob": "YQ==" }
        })));
        assert!(!content_item_has_resource_blob(&json!({
            "type": "resource",
            "resource": { "uri": "u", "text": "hi" }
        })));
        assert!(!content_item_has_resource_blob(&json!({
            "type": "text",
            "text": "hi"
        })));
    }
}
