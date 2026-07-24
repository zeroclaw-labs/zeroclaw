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

pub async fn process_file_entry(
    entry: &FileEntry,
    session_id: &str,
    upload_root: &str,
    is_wss: bool,
    sessions: &SessionStore,
) -> Result<FileEntryResult, JsonRpcError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};

    // 1. Resolve bytes + filename + mime_type.
    let (bytes, filename, mime_type, original_path) = if let Some(ref b64) = entry.data_b64 {
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
        let mime = entry
            .mime_type
            .clone()
            .unwrap_or_else(|| mime_from_filename(&fname));
        (decoded, fname, mime, None)
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
        let bytes = tokio::fs::read(p)
            .await
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cannot read file: {e}")))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(rpc_err(
                INVALID_PARAMS,
                format!(
                    "File exceeds {} MB limit ({} bytes)",
                    MAX_FILE_BYTES / (1024 * 1024),
                    bytes.len()
                ),
            ));
        }
        let fname = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload".to_string());
        let mime = entry
            .mime_type
            .clone()
            .unwrap_or_else(|| mime_from_filename(&fname));
        (bytes, fname, mime, Some(path.clone()))
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

    // 4. Sanitize filename.
    let sanitized = sanitize_filename(&filename);

    // 5. Determine extension + write to workspace.
    let ext = std::path::Path::new(&sanitized)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let storage_name = if ext.is_empty() {
        hex[..16].to_string()
    } else {
        format!("{}.{ext}", &hex[..16])
    };
    let upload_dir = std::path::Path::new(upload_root).join("uploads");
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cannot create upload dir: {e}")))?;
    let dest = upload_dir.join(&storage_name);
    tokio::fs::write(&dest, &bytes)
        .await
        .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cannot write upload: {e}")))?;

    // Canonicalize so the marker always contains an absolute path —
    // upload_root may be relative (e.g. ".") when no path was provided.
    let canonical = tokio::fs::canonicalize(&dest)
        .await
        .unwrap_or_else(|_| dest.clone());
    let canonical_display = canonical.to_string_lossy();
    let workspace_path = strip_windows_verbatim_prefix(&canonical_display).into_owned();

    let kind = attachment_kind(&mime_type);
    let is_clipboard = matches!(entry.source, FileSource::Clipboard);
    let marker = if kind == "IMAGE" {
        let display_path =
            image_marker_display_path(original_path.as_deref(), &workspace_path, is_clipboard);
        format!("[IMAGE:{display_path}]")
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

fn image_marker_display_path<'a>(
    original_path: Option<&'a str>,
    workspace_path: &'a str,
    is_clipboard: bool,
) -> std::borrow::Cow<'a, str> {
    if is_clipboard {
        std::borrow::Cow::Borrowed(workspace_path)
    } else {
        strip_windows_verbatim_prefix(original_path.unwrap_or(workspace_path))
    }
}

/// Derive MIME type from filename extension via `mime_guess`.
/// Falls back to `application/octet-stream` for unknown extensions.
fn mime_from_filename(name: &str) -> String {
    mime_guess::from_path(name)
        .first_or_octet_stream()
        .to_string()
}

/// Map MIME type to attachment kind for markers.
fn attachment_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "IMAGE"
    } else if mime == "application/pdf" {
        "DOCUMENT"
    } else {
        "FILE"
    }
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

    #[test]
    fn mime_from_filename_common_types() {
        assert_eq!(mime_from_filename("photo.png"), "image/png");
        assert_eq!(mime_from_filename("photo.jpg"), "image/jpeg");
        assert_eq!(mime_from_filename("doc.pdf"), "application/pdf");
        assert_eq!(mime_from_filename("data.csv"), "text/csv");
        assert_eq!(
            mime_from_filename("unknown.zzzzz"),
            "application/octet-stream"
        );
        assert_eq!(mime_from_filename("noext"), "application/octet-stream");
    }

    #[test]
    fn attachment_kind_maps_correctly() {
        assert_eq!(attachment_kind("image/png"), "IMAGE");
        assert_eq!(attachment_kind("image/jpeg"), "IMAGE");
        assert_eq!(attachment_kind("image/svg+xml"), "IMAGE");
        assert_eq!(attachment_kind("application/pdf"), "DOCUMENT");
        assert_eq!(attachment_kind("application/zip"), "FILE");
        assert_eq!(attachment_kind("text/plain"), "FILE");
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
    fn path_mode_image_marker_display_path_strips_original_verbatim_prefix() {
        let workspace_path = r"C:\Users\me\.zeroclaw\uploads\copy.png";
        let display_path = image_marker_display_path(
            Some(r"\\?\C:\Users\me\Pictures\source.png"),
            workspace_path,
            false,
        );
        let marker = format!("[IMAGE:{display_path}]");

        assert_eq!(display_path, r"C:\Users\me\Pictures\source.png");
        assert_eq!(marker, r"[IMAGE:C:\Users\me\Pictures\source.png]");
        assert!(!marker.contains(r"\\?\"));
        assert!(!workspace_path.contains(r"\\?\"));
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
            .tools(vec![])
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

        let r = process_file_entry(&entry, "s1", &ws, false, &store)
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

        let r = process_file_entry(&entry, "s1", &ws, false, &store)
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

        let r1 = process_file_entry(&entry, "s1", &ws, false, &store)
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

        let r2 = process_file_entry(&entry2, "s1", &ws, false, &store)
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

        let err = process_file_entry(&entry, "s1", &ws, false, &store)
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("base64"));
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

        let err = process_file_entry(&entry, "s1", &ws, true, &store)
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

        let err = process_file_entry(&entry, "s1", &ws, false, &store)
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

        let r = process_file_entry(&entry, "s1", &ws, false, &store)
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
}
