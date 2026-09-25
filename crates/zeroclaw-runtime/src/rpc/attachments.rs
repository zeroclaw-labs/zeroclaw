//! File attachment processing for the RPC transport.

use super::session::SessionStore;
// FileSource is only referenced from the `#[cfg(test)] mod tests` below,
// which re-imports via `use super::*;`. Quiet the non-test "unused" warning
// without splitting the import into two cfg-gated lines.
#[cfg_attr(not(test), allow(unused_imports))]
use super::types::{FileEntry, FileEntryResult, FileSource};
use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_api::jsonrpc::error_codes::*;

/// Per-file size limit (decoded bytes).
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Per-request total size limit (decoded bytes).
pub const MAX_REQUEST_BYTES: u64 = 20 * 1024 * 1024;

fn rpc_err(code: i32, msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code,
        message: msg.into(),
        data: None,
    }
}

/// A path-mode attachment source, resolved once by the dispatcher and carried
/// intact into the bounded read.
///
/// `target` is the canonical resolved path the agent policy authorized; the
/// read binds to it, never to the raw request spelling. `root` is the approved
/// read root the target sits beneath, or `None` when the policy bounds no root
/// (the final component is then opened beneath its parent handle, which still
/// refuses a swapped final component).
pub struct AttachmentSource {
    pub root: Option<std::path::PathBuf>,
    pub target: std::path::PathBuf,
}

/// Read an attachment source through a directory handle bound to the root the
/// dispatcher authorized (cap-std beneath/no-follow), enforcing type and size on
/// that one handle.
///
/// Re-opening the supplied pathname here would let a writer in the entitled
/// workspace replace a component between the authorization check and the read,
/// redirecting it outside the approved root. `source.root` is `None` when the
/// agent policy bounds no root for the path; the final component is then opened
/// beneath its parent handle, which still refuses a swapped final component.
///
/// The `target` used here is the canonical path the dispatcher authorized, not
/// the raw request spelling, closing the alias-swap window between check and
/// read.
fn read_source_bounded(source: &AttachmentSource) -> Result<Vec<u8>, JsonRpcError> {
    use cap_std::ambient_authority;
    use cap_std::fs::{Dir, OpenOptions};
    use std::io::Read;

    let path = source.target.as_path();

    let too_large = |len: u64| {
        rpc_err(
            INVALID_PARAMS,
            format!(
                "File exceeds {} MB limit ({} bytes)",
                MAX_FILE_BYTES / (1024 * 1024),
                len
            ),
        )
    };

    let (root, rel) = match source.root.as_deref() {
        Some(root) => {
            let rel = path.strip_prefix(root).map_err(|_| {
                rpc_err(
                    INVALID_PARAMS,
                    "Cannot read file: path escapes its approved root",
                )
            })?;
            (root.to_path_buf(), rel.to_path_buf())
        }
        None => {
            let parent = path.parent().ok_or_else(|| {
                rpc_err(
                    INVALID_PARAMS,
                    "Cannot read file: path has no parent directory",
                )
            })?;
            let name = path.file_name().ok_or_else(|| {
                rpc_err(INVALID_PARAMS, "Cannot read file: path has no file name")
            })?;
            (parent.to_path_buf(), std::path::PathBuf::from(name))
        }
    };

    let dir = Dir::open_ambient_dir(&root, ambient_authority())
        .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?;
    // Descend to the file's parent one component at a time, refusing to follow
    // a symlink at ANY intermediate component, then open the final component
    // no-follow beneath that parent handle.
    //
    // `dir.open_with(&rel, nofollow)` only applies `O_NOFOLLOW` to the terminal
    // `open(2)`: cap-std still resolves intermediate components of a multi-part
    // `rel` through its own in-root symlink walk. A writer entitled to the
    // approved root could therefore plant a *relative* in-root symlink as an
    // intermediate directory component (`sub -> ../../elsewhere`) and redirect
    // the read after the dispatcher authorized the canonical target — the
    // final-component nofollow never sees it. An absolute-target swap is
    // already refused because it escapes the cap-std root, which is why the
    // existing escape test passes while this relative-intermediate hole did
    // not. Walking each component with `O_NOFOLLOW | O_DIRECTORY` closes it:
    // every hop is a handle-bound, no-follow directory open, so no symlink on
    // the path — intermediate or terminal — is ever traversed.
    let (parent_dir, file_name) = open_parent_nofollow(&dir, &rel)?;
    // Open without following a final symlink and without blocking on a
    // special-file peer. A FIFO with no writer would make a plain blocking
    // open wait indefinitely on Linux (fifo(7)); `O_NONBLOCK` returns
    // immediately so the regular-file check below can reject it. `nofollow`
    // keeps a swapped final symlink from redirecting the read after the root
    // was authorized.
    let file = {
        let mut opts = OpenOptions::new();
        opts.read(true);
        set_nonblocking_nofollow(&mut opts);
        parent_dir
            .open_with(&file_name, &opts)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?
    };
    let metadata = file
        .metadata()
        .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?;
    if !metadata.is_file() {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Cannot read file: path mode accepts only a regular file",
        ));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(too_large(metadata.len()));
    }
    // One byte over the cap catches a file that grows between stat and read.
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(bytes)
}

/// Walk `rel` beneath `dir` one component at a time, opening every intermediate
/// component as a directory with `O_NOFOLLOW | O_DIRECTORY` so no symlink on the
/// path is ever traversed, and return the parent `Dir` handle plus the final
/// component name for the caller to open no-follow.
///
/// This is the intermediate-component counterpart to the final-component
/// `O_NOFOLLOW`: `Dir::open_with(rel, nofollow)` applies the flag only to the
/// terminal `open(2)`, so a multi-component `rel` still resolves its interior
/// through cap-std's in-root symlink walk. A writer in the entitled root could
/// plant a relative in-root symlink as an interior component and redirect the
/// read after authorization. Descending each hop with a handle-bound no-follow
/// directory open removes that walk entirely: an interior symlink fails the
/// `O_NOFOLLOW` open (ELOOP) instead of being followed.
fn open_parent_nofollow(
    dir: &cap_std::fs::Dir,
    rel: &std::path::Path,
) -> Result<(cap_std::fs::Dir, std::ffi::OsString), JsonRpcError> {
    use std::path::Component;

    // Split off the final component; everything before it is the directory
    // chain we descend no-follow.
    let file_name = match rel.file_name() {
        Some(name) => name.to_os_string(),
        None => {
            return Err(rpc_err(
                INVALID_PARAMS,
                "Cannot read file: path has no file name",
            ));
        }
    };

    let mut current = dir
        .try_clone()
        .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?;

    let parent = rel.parent().unwrap_or_else(|| std::path::Path::new(""));
    for component in parent.components() {
        match component {
            // A normal directory name: descend it no-follow. Any other
            // component kind (RootDir, Prefix, ParentDir, CurDir) has no place
            // in a root-relative attachment path and is refused rather than
            // silently normalized — `..` in particular must never climb out.
            Component::Normal(name) => {
                current = open_child_dir_nofollow(&current, name)?;
            }
            Component::CurDir => {}
            other => {
                return Err(rpc_err(
                    INVALID_PARAMS,
                    format!(
                        "Cannot read file: unexpected path component {other:?} in attachment path"
                    ),
                ));
            }
        }
    }

    Ok((current, file_name))
}

/// Open a single child directory component beneath `parent` with
/// `O_NOFOLLOW | O_DIRECTORY` (Unix) so a symlinked component fails to open
/// instead of being followed, returning the resulting confined `Dir` handle.
#[cfg(unix)]
fn open_child_dir_nofollow(
    parent: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
) -> Result<cap_std::fs::Dir, JsonRpcError> {
    use cap_std::fs::{OpenOptions, OpenOptionsExt};
    let mut opts = OpenOptions::new();
    opts.read(true);
    // O_DIRECTORY: the component must be a real directory (a non-dir fails
    // ENOTDIR). O_NOFOLLOW: a symlinked component fails ELOOP instead of being
    // traversed. O_NONBLOCK guards against a FIFO planted as an interior
    // component wedging the open.
    opts.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK);
    let file = parent
        .open_with(name, &opts)
        .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?;
    Ok(cap_std::fs::Dir::from_std_file(file.into_std()))
}

/// Non-Unix fallback: cap-std's own in-root `open_dir` walk, which refuses
/// components that escape the confined root. `O_NOFOLLOW`/`O_DIRECTORY` have no
/// portable custom-flag equivalent here, and the platforms without them (WSS
/// refuses path mode; local Windows) do not expose the relative-symlink
/// interior-swap hazard on this surface.
#[cfg(not(unix))]
fn open_child_dir_nofollow(
    parent: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
) -> Result<cap_std::fs::Dir, JsonRpcError> {
    parent
        .open_dir(name)
        .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))
}

/// Apply `O_NONBLOCK | O_NOFOLLOW` on Unix so a special-file peer cannot make
/// the open block and a swapped final symlink cannot redirect the read. The
/// cap-std `Dir` handle already refuses any component that escapes the approved
/// root; `O_NOFOLLOW` additionally refuses a symlink *as the final component*,
/// and `O_NONBLOCK` makes a writer-less FIFO fail immediately instead of
/// blocking the async worker (fifo(7)). On non-Unix targets neither flag has an
/// equivalent hazard on the path-mode surface, so the regular-file metadata
/// check remains the type guard.
#[cfg(unix)]
fn set_nonblocking_nofollow(opts: &mut cap_std::fs::OpenOptions) {
    use cap_std::fs::OpenOptionsExt;
    // Use libc's per-target flag values rather than hardcoded octal: the
    // numeric values differ across platforms (e.g. O_NONBLOCK is 0o4000 on
    // Linux but 0o0004 on Darwin/BSD, where 0o4000 is actually O_EXCL), so a
    // literal would silently set the wrong flag off-Linux and let a writer-less
    // FIFO block the worker. O_NONBLOCK: opening a writer-less FIFO returns
    // immediately instead of blocking (fifo(7)). O_NOFOLLOW: refuse a
    // final-component symlink so a retarget after authorization cannot redirect
    // the read. Both are ORed through `custom_flags` into the flags cap-std
    // computes for the confined open.
    opts.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn set_nonblocking_nofollow(_opts: &mut cap_std::fs::OpenOptions) {
    // No FIFO/O_NONBLOCK semantics to guard against on non-Unix path mode
    // (WSS refuses path mode; local Windows has no fifo(7) wait). The
    // regular-file metadata check remains the type guard.
}

pub async fn process_file_entry(
    entry: &FileEntry,
    session_id: &str,
    upload_root: &str,
    is_wss: bool,
    source: Option<&AttachmentSource>,
    sessions: &SessionStore,
) -> Result<FileEntryResult, JsonRpcError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};

    // 1. Resolve bytes + filename + mime_type.
    let (bytes, filename) = if let Some(ref b64) = entry.data_b64 {
        let decoded = STANDARD
            .decode(b64)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Invalid base64: {e}")))?;
        if decoded.len() as u64 > MAX_FILE_BYTES {
            return Err(rpc_err(
                INVALID_PARAMS,
                format!(
                    "File exceeds {} MB limit ({} bytes)",
                    MAX_FILE_BYTES / (1024 * 1024),
                    decoded.len()
                ),
            ));
        }
        let fname = entry.filename.as_deref().unwrap_or("upload").to_string();
        (decoded, fname)
    } else if let Some(ref path) = entry.path {
        if is_wss {
            return Err(rpc_err(
                INVALID_PARAMS,
                "Path mode is not available over WSS; send data_b64 instead",
            ));
        }
        let p = std::path::Path::new(path);
        if !p.is_absolute() {
            return Err(rpc_err(INVALID_PARAMS, "Path must be absolute"));
        }
        // Path mode reads through a source the dispatcher resolved and
        // authorized: the canonical target plus its approved root. An unbound
        // caller (the direct unit-test handlers, which never cross the auth
        // gate) passes `None`; resolve the request here so the read still binds
        // to a canonical target rather than the raw (swappable) request. Fail
        // closed if it cannot be resolved.
        let resolved_source;
        let source = match source {
            Some(source) => source,
            None => {
                let target = std::fs::canonicalize(p)
                    .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?;
                resolved_source = AttachmentSource { root: None, target };
                &resolved_source
            }
        };
        // Judge and read the source through one handle bound to the authorized
        // root: a device such as /dev/zero or a FIFO never reaches end of file
        // (a writer-less FIFO is rejected without blocking), a large file is
        // refused without being pulled into memory, and a component swapped
        // after authorization cannot redirect the read. The open is
        // non-blocking, but the subsequent bounded read is synchronous
        // filesystem work, so run the whole bounded read on a blocking worker
        // to keep it off the async runtime thread.
        let owned = AttachmentSource {
            root: source.root.clone(),
            target: source.target.clone(),
        };
        let bytes = tokio::task::spawn_blocking(move || read_source_bounded(&owned))
            .await
            .map_err(|join| rpc_err(INVALID_PARAMS, format!("Cannot read file: {join}")))??;
        // Name the upload from the canonical target the read was bound to, not
        // the raw request spelling.
        let fname = source
            .target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload".to_string());
        (bytes, fname)
    } else {
        return Err(rpc_err(
            INVALID_PARAMS,
            "Each file entry must have either `data_b64` or `path`",
        ));
    };

    // 2. SHA-256 → ref_id.
    let hash = Sha256::digest(&bytes);
    let hex = format!("{hash:x}");
    let ref_id = format!("sha256:{hex}");

    // 3. Dedup check.
    if let Some(existing) = sessions.get_upload(session_id, &ref_id).await {
        return Ok(FileEntryResult {
            ref_id: existing.ref_id,
            marker: existing.marker,
            workspace_path: existing.workspace_path,
            size_bytes: existing.size_bytes,
            deduplicated: true,
        });
    }

    // 4. Sanitize filename (display only; the on-disk name is the content hash).
    let sanitized = sanitize_filename(&filename);

    // 5. Persist through the shared hardened content-addressed writer: full-digest
    // storage name and a directory-handle-bound, no-follow atomic write. This makes
    // the RPC attachment path and the ACP/MCP blob path share one filesystem owner
    // instead of duplicating decode/hash/naming/persistence with a plain,
    // symlink-following `fs::write`. Marker and session dedup stay RPC-specific.
    let ext = std::path::Path::new(&sanitized)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let dest = zeroclaw_tools::embedded_resource::persist_content_addressed(
        std::path::Path::new(upload_root),
        &bytes,
        &ext,
    )
    .map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))?;
    let workspace_path = strip_windows_verbatim_prefix(&dest.to_string_lossy()).into_owned();

    // IMAGE iff the multimodal loader will actually accept these bytes under
    // this name — the canonical provider-loadable contract from
    // `zeroclaw_api::media` — never a declared MIME. A MIME the loader
    // rejects (image/svg+xml, image/bmp, image/heic) used to earn an
    // [IMAGE:] marker whose payload the provider path then dropped in
    // favour of a "could not be loaded" note, stranding the upload.
    let is_image =
        zeroclaw_api::media::provider_loadable_image_mime_for(&sanitized, &bytes).is_some();
    let marker = if is_image {
        format!("[IMAGE:{workspace_path}]")
    } else {
        // Non-image: prose format with workspace path so the agent can
        // read the file with its tools regardless of transport.
        format!("[Document: {filename}] {workspace_path}")
    };

    let size_bytes = bytes.len() as u64;

    // 7. Index in session upload map.
    sessions
        .insert_upload(
            session_id,
            super::session::UploadEntry {
                ref_id: ref_id.clone(),
                marker: marker.clone(),
                workspace_path: workspace_path.clone(),
                size_bytes,
            },
        )
        .await;

    Ok(FileEntryResult {
        ref_id,
        marker,
        workspace_path,
        size_bytes,
        deduplicated: false,
    })
}

/// Sanitize a filename: strip path separators and null bytes.
fn sanitize_filename(name: &str) -> String {
    name.replace(['/', '\\', '\0'], "_")
}

/// Strip the Windows verbatim (`\\?\`) prefix that `canonicalize` prepends so
/// model-visible file markers contain ordinary local paths.
fn strip_windows_verbatim_prefix(path: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return std::borrow::Cow::Owned(format!(r"\\{rest}"));
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        return std::borrow::Cow::Borrowed(rest);
    }
    std::borrow::Cow::Borrowed(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ffi::OsStr;
    use std::path::{Component, Path};

    fn path_contains_uploads_component(path: &str) -> bool {
        Path::new(path)
            .components()
            .any(|component| matches!(component, Component::Normal(name) if name == OsStr::new("uploads")))
    }

    #[tokio::test]
    async fn svg_becomes_a_document_marker_not_an_image() {
        use base64::{Engine, engine::general_purpose::STANDARD};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        // Declared image MIME cannot earn an [IMAGE:] marker: the provider
        // loader refuses SVG, so promising it to the model strands the file.
        let entry = FileEntry {
            path: None,
            data_b64: Some(STANDARD.encode(b"<svg xmlns='http://www.w3.org/2000/svg'/>")),
            filename: Some("logo.svg".into()),
            mime_type: Some("image/svg+xml".into()),
            source: FileSource::File,
        };
        let r = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();
        assert!(
            r.marker.starts_with("[Document: logo.svg]"),
            "expected document marker, got {}",
            r.marker
        );
    }

    #[tokio::test]
    async fn extensionless_png_bytes_are_an_image_by_magic() {
        use base64::{Engine, engine::general_purpose::STANDARD};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        // No filename, no MIME: the payload's magic bytes alone decide.
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n', 0, 0];
        let entry = FileEntry {
            path: None,
            data_b64: Some(STANDARD.encode(png)),
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let r = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();
        assert!(
            r.marker.starts_with("[IMAGE:"),
            "expected image marker, got {}",
            r.marker
        );
    }

    #[tokio::test]
    async fn declared_image_mime_cannot_widen_acceptance() {
        use base64::{Engine, engine::general_purpose::STANDARD};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        // Text bytes under a text name: a lying image/png MIME changes nothing.
        let entry = FileEntry {
            path: None,
            data_b64: Some(STANDARD.encode(b"plain text")),
            filename: Some("note.txt".into()),
            mime_type: Some("image/png".into()),
            source: FileSource::File,
        };
        let r = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();
        assert!(
            r.marker.starts_with("[Document: note.txt]"),
            "expected document marker, got {}",
            r.marker
        );
    }

    #[test]
    fn sanitize_filename_strips_separators() {
        assert_eq!(sanitize_filename("normal.txt"), "normal.txt");
        assert_eq!(sanitize_filename("path/to/file.txt"), "path_to_file.txt");
        assert_eq!(sanitize_filename("back\\slash.txt"), "back_slash.txt");
        assert_eq!(sanitize_filename("null\0byte.txt"), "null_byte.txt");
    }

    #[test]
    fn strip_windows_verbatim_prefix_keeps_markers_plain() {
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\C:\Users\me\file.png"),
            r"C:\Users\me\file.png"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\UNC\server\share\file.png"),
            r"\\server\share\file.png"
        );
        assert_eq!(
            strip_windows_verbatim_prefix("/tmp/file.png"),
            "/tmp/file.png"
        );
    }

    #[test]
    fn file_source_default_is_file() {
        let source: FileSource = Default::default();
        assert!(matches!(source, FileSource::File));
    }

    #[test]
    fn file_entry_deserialize_data_mode() {
        let v = json!({
            "filename": "screenshot.png",
            "mime_type": "image/png",
            "data_b64": "aGVsbG8="
        });
        let entry: FileEntry = serde_json::from_value(v).unwrap();
        assert_eq!(entry.filename.as_deref(), Some("screenshot.png"));
        assert_eq!(entry.data_b64.as_deref(), Some("aGVsbG8="));
        assert!(entry.path.is_none());
        assert!(matches!(entry.source, FileSource::File));
    }

    #[test]
    fn file_entry_deserialize_path_mode() {
        let v = json!({
            "path": "/home/user/doc.pdf",
            "source": "file"
        });
        let entry: FileEntry = serde_json::from_value(v).unwrap();
        assert_eq!(entry.path.as_deref(), Some("/home/user/doc.pdf"));
        assert!(entry.data_b64.is_none());
    }

    #[test]
    fn file_entry_deserialize_clipboard_source() {
        let v = json!({
            "filename": "paste.png",
            "mime_type": "image/png",
            "data_b64": "aGVsbG8=",
            "source": "clipboard"
        });
        let entry: FileEntry = serde_json::from_value(v).unwrap();
        assert!(matches!(entry.source, FileSource::Clipboard));
    }

    // ── Integration tests against process_file_entry ─────────────

    fn make_session_store(max: usize) -> SessionStore {
        SessionStore::new(
            max,
            std::sync::Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                4, 10, 60,
            )),
        )
    }

    fn make_test_agent() -> crate::agent::agent::Agent {
        use crate::agent::dispatcher::NativeToolDispatcher;

        let mem_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem = std::sync::Arc::from(
            zeroclaw_memory::create_memory(&mem_cfg, &std::env::temp_dir(), None).unwrap(),
        );

        crate::agent::agent::Agent::builder()
            .model_provider(Box::new(StubProvider))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![],
            ))
            .memory(mem)
            .observer(std::sync::Arc::new(crate::observability::NoopObserver {})
                as std::sync::Arc<dyn crate::observability::Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::env::temp_dir())
            .build()
            .unwrap()
    }

    struct StubProvider;

    #[async_trait::async_trait]
    impl zeroclaw_providers::ModelProvider for StubProvider {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn chat(
            &self,
            _: zeroclaw_providers::ChatRequest<'_>,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("stub".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl zeroclaw_api::attribution::Attributable for StubProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "stub"
        }
    }

    async fn setup_store(workspace: &str) -> SessionStore {
        let store = make_session_store(4);
        store
            .insert(
                "s1".into(),
                super::super::session::RpcSession::new(
                    make_test_agent(),
                    "a",
                    workspace,
                    crate::rpc::types::ChatMode::Chat,
                ),
            )
            .await
            .unwrap();
        store
    }

    #[tokio::test]
    async fn clipboard_image() {
        use base64::{Engine, engine::general_purpose::STANDARD};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let png_bytes = b"fake-png-data";
        let entry = FileEntry {
            path: None,
            data_b64: Some(STANDARD.encode(png_bytes)),
            filename: Some("screenshot.png".into()),
            mime_type: Some("image/png".into()),
            source: FileSource::Clipboard,
        };

        let r = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();

        assert!(r.ref_id.starts_with("sha256:"));
        // Clipboard images: marker must contain the workspace path so the
        // multimodal pipeline can load and inline the image bytes. The
        // previous `[IMAGE from clipboard]` marker had no path and silently
        // produced text-only requests (model never saw the image).
        assert!(
            r.marker.starts_with("[IMAGE:") && r.marker.ends_with(']'),
            "marker = {}",
            r.marker
        );
        assert!(
            path_contains_uploads_component(&r.workspace_path),
            "clipboard image marker should reference workspace uploads path: {}",
            r.marker
        );
        assert!(r.marker.contains(&r.workspace_path));
        assert!(!r.deduplicated);
        assert_eq!(r.size_bytes, png_bytes.len() as u64);
        assert!(Path::new(&r.workspace_path).exists());
    }

    /// The dispatcher authorizes a source path, then this reads it. Binding the
    /// read to the authorized root is what stops a name inside that root from
    /// being swapped for a link to a file outside it in between. The dispatcher
    /// resolves the request to a canonical target beneath the approved root and
    /// carries both into the read; a final component later retargeted outside
    /// the root is refused by the root-bound, no-follow open.
    #[tokio::test]
    async fn path_source_swapped_to_escape_the_approved_root_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let root = tmp.path().join("approved");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, b"top secret").unwrap();

        // The authorized name, planted as a link that leaves the root. The
        // canonical target the dispatcher would carry is the in-root name; the
        // root-bound no-follow open refuses to traverse the escaping link.
        let planted = root.join("attachment.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &planted).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, &planted).unwrap();

        let entry = FileEntry {
            path: Some(planted.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        // Source bound to the approved root, with the in-root canonical name as
        // the target — the object-bound decision the dispatcher carries.
        let source = AttachmentSource {
            root: Some(root.clone()),
            target: planted.clone(),
        };
        let err = process_file_entry(&entry, "s1", &ws, false, Some(&source), &store)
            .await
            .expect_err("a source escaping the approved root must be refused");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            !err.message.contains("top secret"),
            "refusal must not carry the file's bytes: {}",
            err.message
        );

        // Control: a regular file inside the root still reads, so the refusal
        // above is the escape and not the binding itself.
        let inside = root.join("ok.txt");
        std::fs::write(&inside, b"fine").unwrap();
        let ok_entry = FileEntry {
            path: Some(inside.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let ok_source = AttachmentSource {
            root: Some(root.clone()),
            target: inside.clone(),
        };
        let ok = process_file_entry(&ok_entry, "s1", &ws, false, Some(&ok_source), &store)
            .await
            .expect("a file inside the approved root must still read");
        assert_eq!(ok.size_bytes, 4);
    }

    /// The interior-component counterpart to the escape test above, reproducing
    /// the exact TOCTOU the reviewer demonstrated. The dispatcher authorizes an
    /// in-root canonical target (`root/public/file.txt`) while `public` is a
    /// real directory. A writer entitled to the root then swaps the intermediate
    /// component for a *relative in-root* symlink pointing at a sibling
    /// (`public -> secrets`) — a sibling that a more-specific `forbidden_paths`
    /// entry would have denied at authorization. The link target never leaves
    /// the filesystem root, so cap-std's confinement does NOT refuse it and the
    /// old single `dir.open_with(rel, nofollow)` followed it, returning
    /// `SECRET-CONTENT` instead of the authorized `PUBLIC-CONTENT`. Only the
    /// component-by-component no-follow descent refuses the swapped interior
    /// component. A genuinely nested in-root file proves the descent does not
    /// over-refuse ordinary nested paths.
    ///
    /// (`process_file_entry` is the read seam; the `forbidden_paths` denial
    /// itself is the dispatcher's authorization job, covered in `dispatch.rs`.
    /// What this asserts is the read never returns an object other than the one
    /// authorization judged, which is what makes the policy denial meaningful.)
    #[cfg(unix)]
    #[tokio::test]
    async fn path_source_with_relative_intermediate_symlink_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let root = tmp.path().join("approved");
        std::fs::create_dir_all(&root).unwrap();

        // An in-root sibling standing in for a more-specific forbidden path.
        // Its target file shares the authorized leaf name so following the
        // swapped interior link would silently return these bytes.
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::write(root.join("secrets").join("file.txt"), b"SECRET-CONTENT").unwrap();

        // The authorized object: a real directory with a real file, canonical
        // at authorization time.
        std::fs::create_dir_all(root.join("public")).unwrap();
        std::fs::write(root.join("public").join("file.txt"), b"PUBLIC-CONTENT").unwrap();

        let target = root.join("public").join("file.txt");
        let source = AttachmentSource {
            root: Some(root.clone()),
            target: target.clone(),
        };

        // TOCTOU swap AFTER the source was authorized: replace the `public`
        // directory with a RELATIVE in-root symlink to the forbidden sibling.
        // The link stays under the fs root, so root confinement alone does not
        // refuse it — the no-follow interior descent must.
        std::fs::rename(root.join("public"), root.join("public.real")).unwrap();
        std::os::unix::fs::symlink("secrets", root.join("public")).unwrap();

        let entry = FileEntry {
            path: Some(target.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let err = process_file_entry(&entry, "s1", &ws, false, Some(&source), &store)
            .await
            .expect_err("a swapped in-root relative interior symlink must be refused");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            !err.message.contains("SECRET"),
            "refusal must not carry the forbidden sibling's bytes: {}",
            err.message
        );

        // Control: a genuinely nested directory inside the root still reads, so
        // the refusal above is the swapped interior link and not the nesting.
        let real_sub = root.join("real");
        std::fs::create_dir_all(&real_sub).unwrap();
        let inside = real_sub.join("ok.txt");
        std::fs::write(&inside, b"fine").unwrap();
        let ok_entry = FileEntry {
            path: Some(inside.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let ok_source = AttachmentSource {
            root: Some(root.clone()),
            target: inside.clone(),
        };
        let ok = process_file_entry(&ok_entry, "s1", &ws, false, Some(&ok_source), &store)
            .await
            .expect("a genuinely nested in-root file must still read");
        assert_eq!(ok.size_bytes, 4);
    }

    #[tokio::test]
    async fn file_pdf() {
        use base64::{Engine, engine::general_purpose::STANDARD};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let entry = FileEntry {
            path: None,
            data_b64: Some(STANDARD.encode(b"%PDF-1.4 fake")),
            filename: Some("report.pdf".into()),
            mime_type: Some("application/pdf".into()),
            source: FileSource::File,
        };

        let r = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();

        // data_b64 mode: non-image uses prose format with workspace path.
        assert!(
            r.marker.starts_with("[Document: report.pdf]"),
            "marker = {}",
            r.marker
        );
        assert!(
            path_contains_uploads_component(&r.workspace_path),
            "marker should include workspace uploads path: {}",
            r.marker
        );
        assert!(r.marker.contains(&r.workspace_path));
        assert!(!r.deduplicated);
    }

    // Consolidation: the RPC attachment writer now persists through the shared
    // hardened content-addressed writer, so the on-disk name is the FULL SHA-256
    // digest, not a 64-bit prefix. Proves the single-filesystem-owner delegation
    // and that RPC no longer uses a collision-feasible truncated identity. The
    // no-follow/handle-bound write property is covered by the shared writer's own
    // regressions in `zeroclaw-tools`.
    #[tokio::test]
    async fn rpc_attachment_stores_under_full_digest_name() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use sha2::{Digest, Sha256};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let bytes = b"rpc-consolidation-bytes";
        let entry = FileEntry {
            path: None,
            data_b64: Some(STANDARD.encode(bytes)),
            filename: Some("doc.pdf".into()),
            mime_type: Some("application/pdf".into()),
            source: FileSource::File,
        };
        let r = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();

        let full_hex = format!("{:x}", Sha256::digest(bytes));
        let name = Path::new(&r.workspace_path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(
            name,
            format!("{full_hex}.pdf"),
            "RPC storage name must be the full digest"
        );
        assert_ne!(
            name,
            format!("{}.pdf", &full_hex[..16]),
            "must not use the old 64-bit prefix identity"
        );
    }

    #[tokio::test]
    async fn deduplication() {
        use base64::{Engine, engine::general_purpose::STANDARD};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let b64 = STANDARD.encode(b"identical-bytes");

        let entry = FileEntry {
            path: None,
            data_b64: Some(b64.clone()),
            filename: Some("img.png".into()),
            mime_type: Some("image/png".into()),
            source: FileSource::Clipboard,
        };

        let r1 = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();
        assert!(!r1.deduplicated);

        let entry2 = FileEntry {
            path: None,
            data_b64: Some(b64),
            filename: Some("img2.png".into()),
            mime_type: Some("image/png".into()),
            source: FileSource::Clipboard,
        };

        let r2 = process_file_entry(&entry2, "s1", &ws, false, None, &store)
            .await
            .unwrap();
        assert!(r2.deduplicated);
        assert_eq!(r1.ref_id, r2.ref_id);
    }

    #[tokio::test]
    async fn malformed_base64() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let entry = FileEntry {
            path: None,
            data_b64: Some("not-valid-base64!!!".into()),
            filename: Some("bad.png".into()),
            mime_type: Some("image/png".into()),
            source: FileSource::File,
        };

        let err = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("base64"));
    }

    #[tokio::test]
    async fn path_mode_refuses_sources_that_are_not_regular_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let mut sources = vec![tmp.path().to_string_lossy().to_string()];
        if cfg!(unix) {
            sources.push("/dev/zero".to_string());
        }
        for source in sources {
            let entry = FileEntry {
                path: Some(source.clone()),
                data_b64: None,
                filename: None,
                mime_type: None,
                source: FileSource::File,
            };
            let err = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                process_file_entry(&entry, "s1", &ws, false, None, &store),
            )
            .await
            .unwrap_or_else(|_| panic!("{source}: the refusal must not wait on the source"))
            .unwrap_err();
            assert_eq!(err.code, INVALID_PARAMS, "{source}");
            assert!(
                err.message.contains("regular file"),
                "{source}: {}",
                err.message
            );
        }
    }

    /// A writer-less FIFO is the precise hazard `O_NONBLOCK` guards: a plain
    /// blocking `open(2)` on it waits indefinitely for a writer (fifo(7)). This
    /// test would hang (and time out) if the nonblocking flag were mis-set —
    /// exactly the Darwin/BSD defect where a hardcoded `0o4000` selects
    /// `O_EXCL` instead of `O_NONBLOCK`. The refusal must return promptly and
    /// as a type refusal, WITHOUT any pathname-only precheck (the open itself
    /// must be what refuses the non-regular file). A normal-file control in the
    /// same workspace proves the guard does not over-refuse.
    #[cfg(unix)]
    #[tokio::test]
    async fn path_mode_refuses_a_writerless_fifo_without_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        // Create a FIFO with no writer inside the workspace.
        let fifo = tmp.path().join("pipe.fifo");
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: c_path is a valid NUL-terminated path; 0o600 is a plain mode.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let fifo_entry = FileEntry {
            path: Some(fifo.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            process_file_entry(&fifo_entry, "s1", &ws, false, None, &store),
        )
        .await
        .expect("the writer-less FIFO refusal must not block on a missing writer")
        .expect_err("a FIFO is not a regular file and must be refused");
        assert_eq!(err.code, INVALID_PARAMS, "{}", err.message);
        assert!(err.message.contains("regular file"), "{}", err.message);

        // Control: an ordinary regular file in the same workspace is accepted.
        let regular = tmp.path().join("ok.txt");
        std::fs::write(&regular, b"hello").unwrap();
        let ok_entry = FileEntry {
            path: Some(regular.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let result = process_file_entry(&ok_entry, "s1", &ws, false, None, &store)
            .await
            .expect("a normal regular file must be accepted by the guard");
        assert_eq!(
            result.size_bytes, 5,
            "the control file's bytes must be read"
        );
    }

    #[tokio::test]
    async fn path_mode_refuses_an_oversized_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;
        let big = tmp.path().join("big.bin");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();

        let entry = FileEntry {
            path: Some(big.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let err = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("limit"), "{}", err.message);
    }

    #[tokio::test]
    async fn rejects_path_over_wss() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let entry = FileEntry {
            path: Some("/home/user/file.txt".into()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };

        let err = process_file_entry(&entry, "s1", &ws, true, None, &store)
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("WSS"));
    }

    #[tokio::test]
    async fn rejects_no_data_and_no_path() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let entry = FileEntry {
            path: None,
            data_b64: None,
            filename: Some("orphan.txt".into()),
            mime_type: None,
            source: FileSource::File,
        };

        let err = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("data_b64"));
    }

    #[tokio::test]
    async fn path_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let file_path = tmp.path().join("testfile.pdf");
        std::fs::write(&file_path, b"%PDF-1.4 test content").unwrap();

        let entry = FileEntry {
            path: Some(file_path.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };

        let r = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();

        assert!(r.ref_id.starts_with("sha256:"));
        // Non-image path mode: prose format with original filename and workspace path.
        assert!(
            r.marker.starts_with("[Document: testfile.pdf]"),
            "marker = {}",
            r.marker
        );
        assert!(
            path_contains_uploads_component(&r.workspace_path),
            "marker should include workspace path: {}",
            r.marker
        );
        assert!(r.marker.contains(&r.workspace_path));
        assert!(!r.deduplicated);
        assert!(Path::new(&r.workspace_path).exists());
    }

    #[tokio::test]
    async fn path_mode_image_marker_survives_original_removal() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir(&ws).unwrap();
        let ws = ws.to_string_lossy().to_string();
        let store = setup_store(&ws).await;

        let source_path = tmp.path().join("temporary.png");
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        std::fs::write(&source_path, png).unwrap();
        let entry = FileEntry {
            path: Some(source_path.to_string_lossy().to_string()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };

        let result = process_file_entry(&entry, "s1", &ws, false, None, &store)
            .await
            .unwrap();
        std::fs::remove_file(&source_path).unwrap();

        assert_eq!(result.marker, format!("[IMAGE:{}]", result.workspace_path));
        assert_eq!(std::fs::read(&result.workspace_path).unwrap(), png);
        assert!(Path::new(&result.workspace_path).exists());

        let prepared = zeroclaw_providers::multimodal::prepare_messages_for_provider(
            &[zeroclaw_api::model_provider::ChatMessage::user(
                result.marker,
            )],
            &zeroclaw_config::schema::MultimodalConfig::default(),
        )
        .await
        .unwrap();
        assert!(prepared.contains_images);
    }
}
