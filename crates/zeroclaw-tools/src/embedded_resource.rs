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

/// Materialize an MCP `type: "image"` payload, which carries a base64 `data`
/// field and no source URI. The on-disk extension must match the bytes: the
/// multimodal loader prefers a path's extension over the decoded bytes' magic,
/// so a wrong extension would mislabel the image to the provider.
///
/// The supported raster type is resolved in this order: the declared `mimeType`,
/// canonicalized to its RFC 6838 essence (lowercased, parameters stripped) and
/// only when it names a type the vision pipeline accepts; otherwise the decoded
/// bytes are sniffed. If neither yields a supported raster type, this degrades
/// with an error and writes nothing — a declared but unsupported/parameterized
/// `image/*` (e.g. `image/bmp`, `image/jpeg; charset=binary`) is never trusted
/// to name the extension, and a case variant like `IMAGE/JPEG` resolves to
/// `.jpg` rather than a `.bin` document.
///
/// Kept separate from [`materialize_resource_blob`] so this URI-absent
/// MIME-to-extension behavior stays confined to the MCP image entry point and
/// does not change the ACP embedded-resource path, whose blobs may also omit a
/// URI.
fn materialize_mcp_image(
    workspace_dir: &Path,
    declared_mime: Option<&str>,
    data_b64: &str,
) -> Result<MaterializedResource, EmbeddedResourceError> {
    let bytes = decode_embedded_blob(data_b64)?;
    // Canonical media-type essence: lowercase, drop any `;`-parameters, trim.
    let declared_essence = declared_mime
        .map(|m| {
            m.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|m| !m.is_empty());
    // Prefer a declared type only when it maps to a supported raster extension;
    // otherwise sniff the bytes. Neither → degrade without writing.
    let mime: String = match declared_essence
        .as_deref()
        .filter(|m| supported_image_ext(m).is_some())
    {
        Some(m) => m.to_string(),
        None => match sniff_image_mime(&bytes) {
            Some(m) => m.to_string(),
            None => {
                return Err(EmbeddedResourceError(
                    "unsupported image media type: no PNG/JPEG/WebP/GIF identified".into(),
                ));
            }
        },
    };
    let ext = supported_image_ext(&mime).expect("mime is supported by construction");
    let filename = format!("upload.{ext}");
    materialize_bytes(workspace_dir, &bytes, &filename, &mime)
}

/// Canonical file extension for the raster image types the vision pipeline
/// accepts (`PROVIDER_IMAGE_MIME_TYPES`: PNG, JPEG, WebP, GIF). `None` for any
/// other media type, so the caller sniffs the bytes or degrades rather than
/// writing a mislabelled file. `mime` must already be the lowercased essence.
fn supported_image_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// Identify a raster image from its leading magic bytes, for MCP image items
/// whose declared `mimeType` is absent or not a supported raster type. Returns
/// one of the vision-accepted `image/*` types, or `None` when the bytes match no
/// supported signature (the caller then degrades without writing).
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
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

/// Whether an MCP tools/call content item is `type: "image"` with a non-empty
/// base64 `data` field — a separate MCP content shape from `resource`+`blob`.
pub(crate) fn content_item_has_mcp_image(item: &serde_json::Value) -> bool {
    item.get("type").and_then(|t| t.as_str()) == Some("image")
        && item
            .get("data")
            .and_then(|d| d.as_str())
            .is_some_and(|s| !s.is_empty())
}

/// The base64 payload a content item will decode, hash, and write to disk, if
/// any: a `resource` with a string `blob`, or a valid `type: "image"` item with
/// non-empty string `data`. Items that will NOT be materialized — audio
/// placeholders, malformed or empty media, and non-binary content — return
/// `None`.
///
/// The aggregate preflight folds over this so its item count and byte estimate
/// cover exactly the items materialized below, and no untrusted content shape
/// can slip past the per-call decode/hash/write budget.
fn materializable_base64(item: &serde_json::Value) -> Option<&str> {
    if content_item_has_resource_blob(item) {
        item.get("resource")
            .and_then(|r| r.get("blob"))
            .and_then(|b| b.as_str())
    } else if content_item_has_mcp_image(item) {
        item.get("data").and_then(|d| d.as_str())
    } else {
        None
    }
}

/// Format an MCP `tools/call` result for the model.
///
/// When `content` contains any `resource` blob or `type: "image"`/`"audio"`
/// item, return the full result as JSON with only the binary payloads redacted:
/// a resource `blob` becomes a Document/IMAGE `materialized` marker; a valid
/// `type: "image"` item is materialized under `{workspace}/uploads/` and
/// rewritten to a text item carrying its `[IMAGE:<path>]` marker, with the item's
/// `annotations`/`_meta` and other non-binary fields preserved; a `type: "audio"`
/// item is redacted to a non-materializing `[audio attachment: <mime>]`
/// placeholder (audio is not materialized in this slice). Raw base64 never
/// survives — a malformed image/audio payload (empty or non-string `data`) is
/// stripped just like a valid one. Every non-binary field (text, `resource_link`,
/// unknown content types, per-item `annotations`, and top-level
/// `structuredContent`/`_meta`/`isError`) is preserved. Results without any
/// binary item keep the existing pretty-printed JSON shape.
///
/// Crate-internal: the only caller is [`crate::mcp_tool::McpToolWrapper`]; the
/// serialized `CallToolResult` from `McpRegistry::call_tool` remains the public
/// surface.
pub(crate) fn format_mcp_tool_result_for_model(
    mut result: serde_json::Value,
    workspace_dir: &Path,
) -> Result<String, EmbeddedResourceError> {
    // Preflight over an immutable borrow: count every binary item that WILL be
    // decoded, hashed, and written — resource blobs AND valid image `data` — and
    // estimate their aggregate decoded size WITHOUT decoding. Two independent
    // per-call bounds guard the untrusted result regardless of content shape: the
    // item count (bounds decode/hash/write attempts, which a byte budget alone
    // does not — empty payloads estimate zero) and the estimated aggregate bytes.
    // No untrusted item shape can bypass this budget. Nothing is cloned; the owned
    // `result` is mutated in place below.
    let (binary_count, aggregate_estimate): (usize, u64) =
        match result.get("content").and_then(|c| c.as_array()) {
            Some(content) => content.iter().fold((0usize, 0u64), |(count, bytes), item| {
                match materializable_base64(item) {
                    Some(b64) => (
                        count + 1,
                        bytes.saturating_add(estimated_decoded_blob_len(b64)),
                    ),
                    None => (count, bytes),
                }
            }),
            None => (0, 0),
        };

    // Process the result if it carries any binary content that must be redacted:
    // a `resource` blob, or a `type: "image"`/`"audio"` item. Image and audio are
    // matched by type alone (not by valid `data`) so a malformed sibling — whose
    // `data` is empty or a non-string — is still stripped rather than passing
    // through with its raw payload intact.
    let has_binary = result
        .get("content")
        .and_then(|c| c.as_array())
        .is_some_and(|content| {
            content.iter().any(|i| {
                content_item_has_resource_blob(i)
                    || matches!(
                        i.get("type").and_then(|t| t.as_str()),
                        Some("image") | Some("audio")
                    )
            })
        });
    if !has_binary {
        return Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()));
    }

    // Which per-call bound was exceeded, if any. When set, every materializable
    // binary item (resource blob or image) is degraded with this marker and
    // nothing is decoded, hashed, or written.
    let over_budget_marker: Option<&str> = if binary_count > MAX_AGGREGATE_BLOB_ITEMS {
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
            "image" => {
                // An MCP image item. Redact the base64 `data` UNCONDITIONALLY —
                // empty, absent, or non-string payloads must never survive into
                // model-facing JSON — and drop the now-superseded `mimeType`. Every
                // other non-binary field (annotations, _meta, and unknown extension
                // fields) is preserved by mutating the object in place.
                let Some(obj) = item.as_object_mut() else {
                    continue;
                };
                let data = match obj.remove("data") {
                    Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s),
                    // Missing / empty / non-string: nothing materializable, and
                    // `data` is already removed so no raw payload can leak.
                    _ => None,
                };
                let declared_mime = obj
                    .remove("mimeType")
                    .and_then(|v| v.as_str().map(str::to_string));
                let marker = if let Some(m) = over_budget_marker {
                    m.to_string()
                } else if let Some(data) = data {
                    match materialize_mcp_image(workspace_dir, declared_mime.as_deref(), &data) {
                        Ok(materialized) => materialized.marker,
                        Err(e) => format!("[attachment unavailable: {e}]"),
                    }
                } else {
                    "[attachment unavailable: malformed image item]".to_string()
                };
                // Convert in place to a text item carrying the marker so the
                // multimodal pipeline (parse_image_markers) lifts [IMAGE:<path>]
                // into a native provider image part. The preserved metadata
                // (annotations, _meta) stays alongside it.
                obj.insert(
                    "type".to_string(),
                    serde_json::Value::String("text".to_string()),
                );
                obj.insert("text".to_string(), serde_json::Value::String(marker));
            }
            "audio" => {
                // Audio is intentionally NOT materialized in this slice: the image
                // intake is split from audio per the accepted issue scope, and no
                // provider resolves an audio path into content parts today — the
                // provider layer rewrites a loadable [AUDIO:<path>] to a
                // placeholder before dispatch, so writing the file delivers
                // nothing. Restore the non-materializing placeholder: strip the
                // base64 `data` and replace it with a concise marker, never
                // touching disk. Other non-binary fields carry through.
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
                        serde_json::Value::String(format!("[audio attachment: {mime}]")),
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
        // A non-resource `image` block materializes and emits `[IMAGE:...]` as
        // a text item, never its base64 data. The old MCP fields (mimeType,
        // materialized) must NOT leak into the model-facing JSON.
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
        assert!(out.contains("[IMAGE:"), "missing IMAGE marker: {out}");
        assert!(
            !out.contains(&img_b64),
            "raw image base64 must not reach the model: {out}"
        );
        // Image item must be replaced with a clean text marker, not leftover MCP fields.
        assert!(
            out.contains(r#""type": "text""#),
            "image item should become text item: {out}"
        );
        assert!(!out.contains("image/png"), "mimeType must not leak: {out}");
        assert!(dir.path().join("uploads").exists());
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

        // Shape gate for MCP `type: "image"` items.
        assert!(content_item_has_mcp_image(&json!({
            "type": "image", "data": "YQ==", "mimeType": "image/png"
        })));
        assert!(!content_item_has_mcp_image(&json!({
            "type": "image", "data": "", "mimeType": "image/png"
        })));
        assert!(!content_item_has_mcp_image(&json!({
            "type": "text", "data": "YQ=="
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

    #[test]
    fn mcp_intake_image_only_without_resource_blobs() {
        // Result with only `type: "image"` items (no resource blobs) still
        // materializes and emits IMAGE markers.
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"img-data");
        let result = json!({
            "content": [
                { "type": "image", "data": b64, "mimeType": "image/jpeg" }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(out.contains("[IMAGE:"), "missing IMAGE marker: {out}");
        // A declared image/jpeg type names the file .jpg so the multimodal
        // loader — which prefers a path's extension over the bytes' magic —
        // reports image/jpeg to the provider.
        assert!(
            out.contains(".jpg"),
            "jpeg image should be stored with a .jpg extension: {out}"
        );
        assert!(dir.path().join("uploads").exists());
    }

    #[test]
    fn mcp_intake_image_without_mimetype_sniffs_jpeg_extension() {
        // A JPEG that arrives without a mimeType must be stored as .jpg (sniffed
        // from the magic bytes), not mislabelled .png by the default — otherwise
        // the extension-first loader would report image/png for JPEG bytes.
        let dir = tempdir().unwrap();
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, jpeg);
        let result = json!({
            "content": [ { "type": "image", "data": b64 } ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(out.contains("[IMAGE:"), "missing IMAGE marker: {out}");
        assert!(
            out.contains(".jpg") && !out.contains(".png"),
            "sniffed JPEG should be stored as .jpg, not .png: {out}"
        );
    }

    #[test]
    fn mcp_intake_audio_degrades_to_placeholder_without_writing() {
        // Audio is NOT materialized in this slice: the base64 `data` is stripped
        // and replaced with a non-materializing `[audio attachment: <mime>]`
        // placeholder, and nothing is written to disk.
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"audio-data");
        let result = json!({
            "content": [
                { "type": "audio", "data": b64, "mimeType": "audio/wav" }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("[audio attachment: audio/wav]"),
            "missing audio placeholder: {out}"
        );
        assert!(
            !out.contains("[AUDIO:"),
            "audio must not materialize: {out}"
        );
        assert!(!out.contains(&b64), "raw audio base64 leaked: {out}");
        assert!(
            !dir.path().join("uploads").exists(),
            "audio must not write a file: {out}"
        );
    }

    #[test]
    fn mcp_intake_image_bad_base64_degrades() {
        // Invalid base64 in an `image` item must degrade per-item, not fail.
        let dir = tempdir().unwrap();
        let result = json!({
            "content": [
                { "type": "image", "data": "%%%", "mimeType": "image/png" },
                { "type": "text", "text": "survivor" }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(out.contains("[attachment unavailable:"));
        assert!(out.contains("survivor"));
    }

    #[test]
    fn mcp_intake_image_oversized_degrades() {
        // Oversized image data must degrade per-item.
        let dir = tempdir().unwrap();
        let big = vec![0u8; (MAX_EMBEDDED_FILE_BYTES as usize) + 1];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &big);
        let result = json!({
            "content": [
                { "type": "image", "data": b64, "mimeType": "image/png" }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(out.contains("[attachment unavailable:"));
    }

    #[test]
    fn mcp_intake_image_only_exceeds_item_budget_writes_nothing() {
        // The aggregate item budget covers image items too: past the limit every
        // image degrades and nothing is decoded, hashed, or written.
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"x");
        let items: Vec<_> = (0..=MAX_AGGREGATE_BLOB_ITEMS)
            .map(|_| json!({ "type": "image", "data": b64, "mimeType": "image/png" }))
            .collect();
        let result = json!({ "content": items });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("too many embedded blobs"),
            "expected item-budget marker: {out}"
        );
        assert!(
            !out.contains("[IMAGE:"),
            "no image may materialize over budget: {out}"
        );
        assert!(
            !dir.path().join("uploads").exists(),
            "nothing may be written over budget: {out}"
        );
    }

    #[test]
    fn mcp_intake_mixed_resource_and_image_exceeds_byte_budget_writes_nothing() {
        // The aggregate BYTE budget spans resource blobs and image data together:
        // a resource blob plus an image whose combined estimate exceeds the limit
        // degrades both, writing nothing.
        let dir = tempdir().unwrap();
        let half = vec![0u8; (MAX_AGGREGATE_BLOB_BYTES as usize) / 2 + 1024];
        let blob = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &half);
        let img = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &half);
        let result = json!({
            "content": [
                {
                    "type": "resource",
                    "resource": { "uri": "file:///a.bin", "blob": blob }
                },
                { "type": "image", "data": img, "mimeType": "image/png" }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("aggregate blob size exceeds limit"),
            "expected byte-budget marker: {out}"
        );
        assert!(
            !out.contains("[IMAGE:"),
            "image must not materialize over budget: {out}"
        );
        assert!(
            !out.contains("[Document:"),
            "blob must not materialize over budget: {out}"
        );
        assert!(
            !dir.path().join("uploads").exists(),
            "nothing may be written over budget: {out}"
        );
    }

    #[test]
    fn mcp_intake_malformed_image_sibling_cannot_retain_data() {
        // A valid image activates formatting; malformed image siblings (non-string
        // or empty `data`) must be stripped to a payload-free marker, never
        // passing through with their raw payload.
        let dir = tempdir().unwrap();
        let good =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"good-image");
        let result = json!({
            "content": [
                { "type": "image", "data": good, "mimeType": "image/png" },
                { "type": "image", "data": { "nested": "leak-me" }, "mimeType": "image/png" },
                { "type": "image", "data": "", "mimeType": "image/png" }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("[IMAGE:"),
            "valid image should materialize: {out}"
        );
        assert!(
            !out.contains("leak-me"),
            "non-string data on a malformed sibling leaked: {out}"
        );
        assert!(
            out.contains("malformed image item"),
            "malformed sibling should degrade to a payload-free marker: {out}"
        );
        assert!(
            !out.contains("image/png"),
            "mimeType must not survive on any image item: {out}"
        );
    }

    #[test]
    fn mcp_intake_image_conversion_preserves_annotations_and_meta() {
        // Converting an image item to its text marker must keep the item's
        // non-binary metadata (annotations, _meta) while dropping only the binary
        // `data` and superseded `mimeType`.
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"img");
        let result = json!({
            "content": [
                {
                    "type": "image",
                    "data": b64,
                    "mimeType": "image/png",
                    "annotations": { "audience": ["user"] },
                    "_meta": { "source": "tool-x" }
                }
            ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(out.contains("[IMAGE:"), "missing IMAGE marker: {out}");
        assert!(
            out.contains(r#""type": "text""#),
            "image should convert to a text item: {out}"
        );
        assert!(
            out.contains("annotations") && out.contains("audience"),
            "annotations dropped: {out}"
        );
        assert!(
            out.contains("_meta") && out.contains("tool-x"),
            "_meta dropped: {out}"
        );
        assert!(
            !out.contains("image/png"),
            "mimeType must not survive: {out}"
        );
        assert!(!out.contains(&b64), "raw data leaked: {out}");
    }

    #[test]
    fn acp_uri_absent_blob_keeps_master_filename_semantics() {
        // The MIME-to-extension behavior is confined to the MCP image entry
        // point. The shared materialize_resource_blob (used by ACP) keeps its
        // master semantics for a URI-less blob: a non-image is a Document named
        // upload.bin, and an image still resolves to an [IMAGE:] marker so ACP
        // image delivery is unaffected.
        let dir = tempdir().unwrap();
        let pdf =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"%PDF-1.7 body");
        let doc =
            materialize_resource_blob(dir.path(), None, Some("application/pdf"), &pdf).unwrap();
        assert!(
            doc.marker.contains("[Document:"),
            "uri-less non-image should be a Document: {}",
            doc.marker
        );
        assert!(
            doc.abs_path.to_string_lossy().ends_with(".bin"),
            "uri-less blob keeps the .bin name, not a MIME-derived extension: {:?}",
            doc.abs_path
        );

        let img = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"img-bytes");
        let out = materialize_resource_blob(dir.path(), None, Some("image/png"), &img).unwrap();
        assert!(
            out.marker.starts_with("[IMAGE:"),
            "ACP image blob still materializes as IMAGE: {}",
            out.marker
        );
    }

    #[test]
    fn mcp_intake_image_uppercase_mime_normalized_to_jpg() {
        // RFC 6838 media types are case-insensitive: IMAGE/JPEG must resolve to a
        // .jpg IMAGE marker, not a .bin document.
        let dir = tempdir().unwrap();
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, jpeg);
        let result = json!({
            "content": [ { "type": "image", "data": b64, "mimeType": "IMAGE/JPEG" } ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("[IMAGE:"),
            "uppercase mime should still be an image: {out}"
        );
        assert!(
            out.contains(".jpg") && !out.contains(".bin"),
            "IMAGE/JPEG should normalize to .jpg: {out}"
        );
    }

    #[test]
    fn mcp_intake_image_parameterized_mime_uses_essence() {
        // A `; charset=…` parameter is not part of the media-type essence.
        let dir = tempdir().unwrap();
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png);
        let result = json!({
            "content": [ { "type": "image", "data": b64, "mimeType": "image/png; charset=binary" } ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("[IMAGE:"),
            "parameterized mime should still be an image: {out}"
        );
        assert!(
            out.contains(".png") && !out.contains(".bin"),
            "parameterized image/png should resolve to .png: {out}"
        );
    }

    #[test]
    fn mcp_intake_image_untabled_mime_falls_back_to_sniff() {
        // A declared but unsupported image type (image/bmp) must not name the
        // extension; the real PNG bytes are sniffed and stored as .png.
        let dir = tempdir().unwrap();
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png);
        let result = json!({
            "content": [ { "type": "image", "data": b64, "mimeType": "image/bmp" } ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("[IMAGE:"),
            "sniffed PNG should be an image: {out}"
        );
        assert!(
            out.contains(".png") && !out.contains(".bin"),
            "untabled declaration should fall back to sniffed .png: {out}"
        );
    }

    #[test]
    fn mcp_intake_image_unsupported_and_unsniffable_degrades_without_write() {
        // Declared type has no supported extension AND the bytes match no known
        // raster signature: degrade to an unavailable marker, write nothing.
        let dir = tempdir().unwrap();
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"this is not a raster image",
        );
        let result = json!({
            "content": [ { "type": "image", "data": b64, "mimeType": "image/bmp" } ]
        });
        let out = format_mcp_tool_result_for_model(result, dir.path()).unwrap();
        assert!(
            out.contains("[attachment unavailable:"),
            "unsupported unidentifiable image should degrade: {out}"
        );
        assert!(
            !out.contains("[IMAGE:"),
            "must not emit an image marker: {out}"
        );
        assert!(
            !dir.path().join("uploads").exists(),
            "must not write a mislabelled file: {out}"
        );
    }
}
