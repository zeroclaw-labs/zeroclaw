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
}
