//! Materialize embedded `resource.blob` payloads into the session workspace.
//! Store-agnostic: no RPC `SessionStore` / `file/attach`. Shared by ACP inbound
//! and MCP tools/call postprocessing.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Per-file decoded size limit for embedded blobs (matches RPC attach / ACP).
pub const MAX_EMBEDDED_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Estimated aggregate decoded size limit for all embedded blobs in a single
/// `tools/call` result. Beyond this, every resource blob is replaced with an
/// aggregate-limit marker and no file is written.
///
/// `pub(crate)` so the MCP transport can size its encoded response ceiling to
/// this decoded budget plus base64 and JSON overhead, keeping the two layers in
/// lockstep instead of duplicating the literal.
pub(crate) const MAX_AGGREGATE_BLOB_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum number of embedded resource blobs materialized from a single
/// `tools/call` result. A byte budget alone does not bound the per-item work: an
/// untrusted server can return a large array of empty or tiny blobs whose
/// estimated total stays under [`MAX_AGGREGATE_BLOB_BYTES`], yet still forces one
/// decode + hash + filesystem write attempt per item. This caps the item count
/// independently; beyond it, every resource blob is degraded with a marker and
/// nothing is written.
const MAX_AGGREGATE_BLOB_ITEMS: usize = 64;

/// Estimated decoded byte length of a base64 `blob` string, computed without
/// decoding. Base64 encodes 3 bytes per 4 characters; the trailing `=` padding
/// (0-2 chars) is not data, so it is subtracted. Counting padding as data (a
/// plain `len * 3 / 4`) overestimates by up to 2 bytes, which wrongly rejects a
/// blob whose real decoded size is exactly at the limit. A malformed length that
/// is not a multiple of 4 undercounts the final partial group, which is safe:
/// the per-file decode still enforces the hard cap on the real bytes.
///
/// Only the two canonical padding positions are subtracted. Valid standard
/// base64 has at most two trailing `=`, so a server-controlled string made
/// mostly or entirely of `=` must not estimate as near-zero: without this cap
/// its full `=` run would be subtracted, the aggregate byte gate would be
/// bypassed, and `Engine::decode()` would still allocate a buffer sized from the
/// original encoded length (~3/4 of it) before rejecting the input. Counting at
/// most two padding characters keeps such a blob's estimate proportional to its
/// length so the aggregate gate degrades it before any decode allocation.
fn estimated_decoded_blob_len(blob: &str) -> u64 {
    let len = blob.len() as u64;
    let pad = blob
        .bytes()
        .rev()
        .take_while(|&b| b == b'=')
        .take(2)
        .count() as u64;
    ((len / 4) * 3).saturating_sub(pad)
}

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

/// Normalize a filesystem extension to a strict URI-safe token, or drop it.
/// Keeps only a short run of ASCII alphanumerics (lowercased); anything with
/// reserved, percent, whitespace, or control characters, or an over-long value,
/// yields no extension. This keeps the `<sha16>.<ext>` identity always safe to
/// embed verbatim in an `attachment://` citation URI.
fn safe_ext(ext: &str) -> Option<String> {
    let ext = ext.trim();
    if ext.is_empty() || ext.len() > 16 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        None
    } else {
        Some(ext.to_ascii_lowercase())
    }
}

/// `<sha256>` (or `<sha256>.<ext>`) given the full lowercase hex SHA-256 digest.
/// The full 256-bit digest is the identity: a truncated prefix (e.g. 64 bits)
/// would let two attacker-controlled blobs collide onto one name. The extension
/// is normalized through [`safe_ext`], so a caller-supplied filename can never
/// leak reserved/unsafe characters into the identity.
fn hash_name(hex: &str, ext: &str) -> String {
    match safe_ext(ext) {
        Some(ext) => format!("{hex}.{ext}"),
        None => hex.to_string(),
    }
}

/// Content-addressed identity `<sha256>` / `<sha256>.<ext>` derived from raw bytes
/// (the full hex SHA-256 digest). Shared by blob materialization and outbound
/// delivery URIs so both use the same opaque, URI-safe, collision-resistant name
/// — the identity is the full 256-bit digest and depends on content, never on a
/// caller-supplied filename, so distinct content never aliases one name and
/// reserved characters can't leak.
pub fn content_hash_name(bytes: &[u8], ext: &str) -> String {
    hash_name(&format!("{:x}", Sha256::digest(bytes)), ext)
}

/// Decode a base64 embedded blob and enforce the size cap, WITHOUT writing
/// anything. Lets a caller validate every prompt part up front so an invalid
/// later part cannot leave earlier parts already materialized on disk.
pub fn decode_embedded_blob(blob_b64: &str) -> Result<Vec<u8>, EmbeddedResourceError> {
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
    Ok(bytes)
}

/// Decode `blob_b64`, enforce size limits, write under `{workspace}/uploads/`,
/// and return a prompt marker (`[Document: …]` or `[IMAGE:…]`). Thin base64
/// front door over [`materialize_bytes`].
pub fn materialize_resource_blob(
    workspace_dir: &Path,
    uri: Option<&str>,
    mime_type: Option<&str>,
    blob_b64: &str,
) -> Result<MaterializedResource, EmbeddedResourceError> {
    let bytes = decode_embedded_blob(blob_b64)?;

    let filename = sanitize_filename(&filename_from_uri(uri));
    let mime = mime_type
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| mime_from_filename(&filename));

    materialize_bytes(workspace_dir, &bytes, &filename, &mime)
}

/// Persist already-read `bytes` as a content-addressed file under
/// `{workspace}/uploads/<sha16>.<safe-ext>` and return where it landed. The
/// on-disk name is the content hash (never a caller-supplied filename), the
/// write is no-follow (symlinks at the destination are dropped, not followed),
/// confinement is checked before the write and re-verified after it, and the
/// input is size-capped. `filename`/`mime` are display metadata only. Used by
/// both inbound blob intake and outbound `deliver_file`, so ACP consumers can
/// read this workspace-internal, content-named copy rather than re-opening a
/// caller-supplied path.
pub fn materialize_bytes(
    workspace_dir: &Path,
    bytes: &[u8],
    filename: &str,
    mime: &str,
) -> Result<MaterializedResource, EmbeddedResourceError> {
    let ext = Path::new(filename)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let abs_path = persist_content_addressed(workspace_dir, bytes, &ext)?;

    let abs_display = strip_windows_verbatim_prefix(&abs_path.to_string_lossy()).into_owned();
    let marker = if mime.starts_with("image/") {
        format!("[IMAGE:{abs_display}]")
    } else {
        format!("[Document: {filename}] {abs_display}")
    };

    Ok(MaterializedResource {
        abs_path,
        marker,
        mime_type: mime.to_string(),
        filename: filename.to_string(),
    })
}

/// Persist `bytes` as a content-addressed file `<sha256>.<safe-ext>` under
/// `{workspace}/uploads/` and return its absolute on-disk path. This is the
/// single hardened persistence substrate shared by ACP/MCP blob intake, outbound
/// `deliver_file`, and the RPC attachment writer.
///
/// Every filesystem operation is bound to a directory handle opened once
/// ([`cap_std::fs::Dir`], beneath/no-follow semantics): the uploads handle is
/// resolved a single time, and temp creation, rename, dedup reads, and the final
/// install all go through that handle rather than re-resolving a pathname. A
/// directory or symlink swapped in after the handle is opened therefore cannot
/// redirect any write or read outside the workspace — closing the check/act
/// window a post-write `canonicalize` could only detect after the fact.
///
/// The on-disk name is the full-digest content hash (never a caller-supplied
/// filename), a symlink pre-planted at the destination is dropped (not followed),
/// dedup verifies bytes rather than trusting a length match, and the input is
/// size-capped.
pub fn persist_content_addressed(
    workspace_dir: &Path,
    bytes: &[u8],
    ext: &str,
) -> Result<PathBuf, EmbeddedResourceError> {
    use cap_std::ambient_authority;
    use cap_std::fs::Dir;

    if bytes.len() as u64 > MAX_EMBEDDED_FILE_BYTES {
        return Err(EmbeddedResourceError(format!(
            "Embedded resource exceeds {} MB limit ({} bytes)",
            MAX_EMBEDDED_FILE_BYTES / (1024 * 1024),
            bytes.len()
        )));
    }

    let hex = format!("{:x}", Sha256::digest(bytes));
    let storage_name = hash_name(&hex, ext);

    // Bind to the workspace via a handle opened once. Every op below is relative
    // to this handle, so a swapped pathname cannot escape it.
    let ws = Dir::open_ambient_dir(workspace_dir, ambient_authority())
        .map_err(|e| EmbeddedResourceError(format!("Cannot open workspace dir: {e}")))?;

    // A symlinked `uploads/` would redirect every write outside the workspace.
    // Refuse it, then create and open uploads as a bound sub-handle. `open_dir`
    // itself refuses to traverse a symlink that escapes the workspace.
    if let Ok(meta) = ws.symlink_metadata("uploads")
        && meta.is_symlink()
    {
        return Err(EmbeddedResourceError(
            "uploads path is a symlink; refusing to materialize blob".into(),
        ));
    }
    ws.create_dir_all("uploads")
        .map_err(|e| EmbeddedResourceError(format!("Cannot create upload dir: {e}")))?;
    let uploads = ws
        .open_dir("uploads")
        .map_err(|e| EmbeddedResourceError(format!("Cannot open upload dir: {e}")))?;

    write_blob_content_addressed(&uploads, &storage_name, &hex, bytes)?;

    // Absolute path for display / for telling consumers where the content-named
    // copy lives. The write itself was confined by the handle; this join is only
    // used to build markers and locate the file, never to authorize the write.
    let abs = std::fs::canonicalize(workspace_dir)
        .map(|w| w.join("uploads").join(&storage_name))
        .unwrap_or_else(|_| workspace_dir.join("uploads").join(&storage_name));
    Ok(abs)
}

/// Install `bytes` as `dest_name` under the already-verified `uploads` directory
/// handle, content-addressed and atomically, without ever re-resolving a pathname.
///
/// All operations go through `uploads` (a bound [`cap_std::fs::Dir`]): a symlink
/// pre-planted at `dest_name` is dropped (not followed); dedup verifies the bytes
/// on disk rather than trusting a length match; the write goes to a uniquely-named
/// temp opened with `create_new` (so a pre-planted temp symlink is refused, not
/// followed) and is renamed over `dest_name`. Because the handle is bound before
/// any write, a directory swapped in at the `uploads` pathname afterwards cannot
/// redirect the temp creation, rename, or dedup read outside the workspace.
fn write_blob_content_addressed(
    uploads: &cap_std::fs::Dir,
    dest_name: &str,
    hex: &str,
    bytes: &[u8],
) -> Result<(), EmbeddedResourceError> {
    use cap_std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Never follow a symlink sitting at the destination. Drop it; the atomic
    // rename below installs a fresh regular file in its place.
    if let Ok(meta) = uploads.symlink_metadata(dest_name)
        && meta.is_symlink()
    {
        uploads.remove_file(dest_name).map_err(|e| {
            EmbeddedResourceError(format!("Cannot clear symlink at upload dest: {e}"))
        })?;
    }

    // Content-addressed dedup, but verify content rather than trusting a length
    // match: an attacker-substituted file of equal length must not be reused.
    let already_present = matches!(
        uploads.symlink_metadata(dest_name),
        Ok(meta) if meta.is_file()
            && meta.len() == bytes.len() as u64
            && uploads.read(dest_name).is_ok_and(|b| b.as_slice() == bytes)
    );
    if already_present {
        return Ok(());
    }

    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = format!(".tmp-{}-{seq}-{}", std::process::id(), &hex[..16]);

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    let mut file = uploads
        .open_with(&tmp, &opts)
        .map_err(|e| EmbeddedResourceError(format!("Cannot create upload temp file: {e}")))?;
    let write_result = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| EmbeddedResourceError(format!("Cannot write upload: {e}")));
    if let Err(e) = write_result {
        let _ = uploads.remove_file(&tmp);
        return Err(e);
    }
    drop(file);

    if let Err(e) = uploads.rename(&tmp, uploads, dest_name) {
        let _ = uploads.remove_file(&tmp);
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
    mut result: serde_json::Value,
    workspace_dir: &Path,
) -> Result<String, EmbeddedResourceError> {
    // Preflight over an immutable borrow: count resource blobs and estimate their
    // aggregate decoded size WITHOUT decoding. Two independent per-call bounds
    // guard the untrusted result: the item count (bounds decode/hash/write
    // attempts, which a byte budget alone does not — empty blobs estimate zero)
    // and the estimated aggregate bytes. Nothing is cloned; the owned `result` is
    // mutated in place below.
    let (blob_count, aggregate_estimate): (usize, u64) =
        match result.get("content").and_then(|c| c.as_array()) {
            Some(content) => content
                .iter()
                .filter(|i| content_item_has_resource_blob(i))
                .fold((0usize, 0u64), |(count, bytes), item| {
                    let blob = item
                        .get("resource")
                        .and_then(|r| r.get("blob"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    (
                        count + 1,
                        bytes.saturating_add(estimated_decoded_blob_len(blob)),
                    )
                }),
            None => (0, 0),
        };

    if blob_count == 0 {
        return Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()));
    }

    // Which per-call bound was exceeded, if any. When set, every resource blob is
    // degraded with this marker and nothing is decoded, hashed, or written.
    let over_budget_marker: Option<&str> = if blob_count > MAX_AGGREGATE_BLOB_ITEMS {
        Some("[attachment unavailable: too many embedded blobs in one result]")
    } else if aggregate_estimate > MAX_AGGREGATE_BLOB_BYTES {
        Some("[attachment unavailable: aggregate blob size exceeds limit]")
    } else {
        None
    };

    // Preserve the entire result and redact ONLY binary payloads. This keeps the
    // machine-readable provenance the model (and downstream tooling) may rely on:
    // structuredContent, _meta, per-item annotations, isError, text, resource_link,
    // and unknown content types all survive; only base64 blob/data are removed.
    let Some(items) = result.get_mut("content").and_then(|c| c.as_array_mut()) else {
        return Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()));
    };
    for item in items.iter_mut() {
        let typ = item
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        match typ.as_str() {
            "resource" => {
                let Some(res) = item.get_mut("resource").and_then(|r| r.as_object_mut()) else {
                    continue;
                };
                // A `resource` without a string `blob` (e.g. resource_link) carries
                // through untouched.
                if res.get("blob").and_then(|b| b.as_str()).is_none() {
                    continue;
                }
                // Small metadata; cloning these is not the base64 payload.
                let uri = res.get("uri").and_then(|v| v.as_str()).map(str::to_string);
                let mime = res
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // Take OWNERSHIP of the base64 blob string instead of copying it:
                // over-budget simply drops it (no decode/hash/write, no copy), and
                // the accepted path materializes from the owned string.
                let blob = match res.remove("blob") {
                    Some(serde_json::Value::String(blob)) => blob,
                    // Non-string blob can't happen after the check above; if it
                    // somehow does, the field is already removed and we degrade.
                    _ => String::new(),
                };
                // Over an exceeded per-call bound, degrade without touching disk.
                // Otherwise degrade per-item: one malformed/oversized blob must
                // not fail the whole result or leak base64.
                let marker = if let Some(m) = over_budget_marker {
                    m.to_string()
                } else {
                    match materialize_resource_blob(
                        workspace_dir,
                        uri.as_deref(),
                        mime.as_deref(),
                        &blob,
                    ) {
                        Ok(materialized) => materialized.marker,
                        Err(e) => format!("[attachment unavailable: {e}]"),
                    }
                };
                res.insert(
                    "materialized".to_string(),
                    serde_json::Value::String(marker),
                );
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

    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()))
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

    // Security (#2): the write is bound to the uploads directory HANDLE, not its
    // pathname. A directory swapped in at the `uploads` path after the handle is
    // opened must not redirect the write outside the workspace. A pathname-based
    // writer (canonicalize then write by path) would follow the swapped symlink
    // and escape; this proves the handle-bound writer does not.
    #[cfg(unix)]
    #[test]
    fn write_is_bound_to_the_handle_not_the_uploads_pathname() {
        use cap_std::ambient_authority;
        use cap_std::fs::Dir;

        let ws = tempdir().unwrap();
        let uploads_path = ws.path().join("uploads");
        std::fs::create_dir(&uploads_path).unwrap();

        // Bind a handle to the REAL uploads directory (its inode).
        let ws_dir = Dir::open_ambient_dir(ws.path(), ambient_authority()).unwrap();
        let uploads = ws_dir.open_dir("uploads").unwrap();

        // Simulate a directory swap AFTER binding: move the real uploads aside and
        // repoint the `uploads` pathname at a directory outside the workspace.
        let outside = tempdir().unwrap();
        let moved = ws.path().join("uploads_real");
        std::fs::rename(&uploads_path, &moved).unwrap();
        std::os::unix::fs::symlink(outside.path(), &uploads_path).unwrap();

        let bytes = b"handle-bound-bytes";
        let hex = format!("{:x}", Sha256::digest(bytes));
        write_blob_content_addressed(&uploads, "file.bin", &hex, bytes).unwrap();

        // Bytes landed in the ORIGINAL uploads inode (now at `moved`), never in the
        // swapped-in outside directory.
        assert_eq!(std::fs::read(moved.join("file.bin")).unwrap(), bytes);
        assert!(
            std::fs::read(outside.path().join("file.bin")).is_err(),
            "write escaped the workspace through the swapped uploads pathname"
        );
    }

    #[test]
    fn identity_uses_full_digest_not_16_char_prefix() {
        // Two DISTINCT full SHA-256 digests that share the first 16 hex chars must
        // map to different storage/citation identities. A 64-bit prefix identity
        // would collapse them onto one dest + one URI (feasible ~2^32 collision
        // work for an attacker who controls the bytes).
        let shared = "0123456789abcdef";
        let a = format!("{shared}{}", "a".repeat(48)); // 64 hex chars
        let b = format!("{shared}{}", "b".repeat(48));
        assert_eq!(a.len(), 64);
        assert_ne!(
            hash_name(&a, "pdf"),
            hash_name(&b, "pdf"),
            "distinct full digests sharing a 16-char prefix must not collapse"
        );
        // The identity carries the full digest, not a truncation.
        assert!(hash_name(&a, "pdf").starts_with(&a));
    }

    #[test]
    fn content_hash_name_keeps_the_identity_uri_safe() {
        let b = b"x";
        let hash = format!("{:x}", Sha256::digest(b));
        let stem = hash.as_str();
        // Safe extensions are kept (lowercased); the stem is the full content hash.
        assert_eq!(content_hash_name(b, "pdf"), format!("{stem}.pdf"));
        assert_eq!(content_hash_name(b, "PDF"), format!("{stem}.pdf"));
        // Reserved / percent / space / control / over-long extensions are dropped,
        // so the attachment URI can never carry an unsafe character.
        for bad in ["a?b", "a#b", "a%b", "a b", "a\nb", "a/b", &"a".repeat(17)] {
            assert_eq!(
                content_hash_name(b, bad),
                stem.to_string(),
                "unsafe ext {bad:?} must be dropped from the identity"
            );
        }
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
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

    #[test]
    fn mcp_intake_within_aggregate_budget() {
        // Multiple blobs whose total is within the aggregate limit all materialize.
        let dir = tempdir().unwrap();
        let blob1 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"small payload a",
        );
        let blob2 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"small payload b",
        );
        let result = json!({
            "content": [
                {
                    "type": "resource",
                    "resource": { "uri": "file:///a.txt", "blob": blob1 }
                },
                {
                    "type": "resource",
                    "resource": { "uri": "file:///b.txt", "blob": blob2 }
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(out.contains("[Document: a.txt]"));
        assert!(out.contains("[Document: b.txt]"));
        assert!(dir.path().join("uploads").exists());
    }

    #[test]
    fn mcp_intake_exceeds_aggregate_budget() {
        // When aggregate estimated size exceeds the limit, all resource blobs
        // are degraded with an aggregate-limit marker and nothing is written.
        let dir = tempdir().unwrap();
        let chunk = vec![0u8; (MAX_AGGREGATE_BLOB_BYTES as usize) / 2 + 1]; // > 5 MiB each
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &chunk);
        let result = json!({
            "content": [
                {
                    "type": "resource",
                    "resource": { "uri": "file:///a.bin", "blob": b64.clone() }
                },
                {
                    "type": "resource",
                    "resource": { "uri": "file:///b.bin", "blob": b64.clone() }
                },
                {
                    "type": "text",
                    "text": "survivor"
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(!dir.path().join("uploads").exists());
        assert!(out.contains("aggregate blob size exceeds limit"));
        assert!(!out.contains("[Document:"));
        assert!(out.contains("survivor"));
    }

    #[test]
    fn mcp_intake_rejects_excessive_empty_blob_items() {
        // An untrusted server can return a large array of empty blobs. Each
        // estimates zero decoded bytes, so the byte budget alone never trips;
        // only the item-count bound stops the per-item decode/hash/write work.
        // Every blob must be degraded and nothing written to disk.
        let dir = tempdir().unwrap();
        let items: Vec<_> = (0..MAX_AGGREGATE_BLOB_ITEMS + 1)
            .map(|i| {
                json!({
                    "type": "resource",
                    "resource": { "uri": format!("file:///f{i}.bin"), "blob": "" }
                })
            })
            .collect();
        let result = json!({ "content": items });

        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            !dir.path().join("uploads").exists(),
            "an over-count result must not write any file"
        );
        assert!(out.contains("too many embedded blobs in one result"));
        assert!(!out.contains("[Document:"));
    }

    #[test]
    fn mcp_intake_materializes_blob_at_exact_aggregate_limit() {
        // A single blob whose decoded size is exactly the aggregate limit must be
        // materialized, not rejected. Estimating base64 as `len * 3 / 4` counts
        // the `=` padding as data and pushes an exact-limit blob two bytes over,
        // wrongly tripping the gate; subtracting padding keeps it at the limit.
        let dir = tempdir().unwrap();
        let payload = vec![0u8; MAX_AGGREGATE_BLOB_BYTES as usize];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &payload);
        let result = json!({
            "content": [
                {
                    "type": "resource",
                    "resource": { "uri": "file:///big.bin", "blob": b64 }
                }
            ]
        });

        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            !out.contains("aggregate blob size exceeds limit"),
            "an exact-limit blob must not be rejected by the aggregate gate"
        );
        assert!(out.contains("[Document: big.bin]"));
        assert!(dir.path().join("uploads").exists());
    }

    #[test]
    fn mcp_intake_rejects_malformed_all_padding_blob_without_decoding() {
        // A server-controlled blob made entirely of `=` is non-canonical base64.
        // If every trailing `=` were subtracted, its estimate would collapse to
        // zero and bypass the aggregate byte gate, yet `Engine::decode()` would
        // still allocate ~3/4 of the encoded length before rejecting the input.
        // Capping the subtracted padding at two keeps the estimate proportional
        // to the length, so the aggregate gate degrades it with a marker and no
        // decode, hash, or filesystem write occurs.
        let dir = tempdir().unwrap();
        let blob = "=".repeat((MAX_AGGREGATE_BLOB_BYTES as usize) * 2);
        let result = json!({
            "content": [
                {
                    "type": "resource",
                    "resource": { "uri": "file:///malformed.bin", "blob": blob }
                }
            ]
        });

        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            !dir.path().join("uploads").exists(),
            "a malformed all-padding blob must not be decoded or written"
        );
        assert!(out.contains("aggregate blob size exceeds limit"));
        assert!(!out.contains("[Document:"));
    }
}
