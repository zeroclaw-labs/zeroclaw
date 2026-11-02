//! Policy-checked loading of workspace files that a tool sends as outbound
//! channel attachments.

use super::filesystem_boundary::{
    FilesystemBoundaryError, open_absolute_dir_nofollow, open_file_nofollow,
};
use std::io::Read;
use std::path::{Component, Path};
use zeroclaw_api::media::MediaAttachment;
use zeroclaw_config::policy::SecurityPolicy;

/// Load one file named by a tool argument as an outbound attachment.
///
/// The path passes the same read policy as `file_read`: lexical checks, the
/// workspace resolver, canonicalization and the readable-roots check. Runtime
/// configuration files are refused even where they are readable. The file is
/// then opened once without following symlinks along its canonical path, and
/// at most `max_bytes + 1` bytes are read from that handle, so the size limit
/// holds for the bytes actually sent. The attachment is built from those bytes
/// directly; nothing re-opens the path afterwards.
///
/// Every error names `path` as the caller gave it.
pub(crate) fn read_workspace_attachment(
    security: &SecurityPolicy,
    path: &str,
    max_bytes: u64,
) -> Result<MediaAttachment, String> {
    if path.trim().is_empty() {
        return Err(tool_text("tool-outbound-attachment-error-empty-path"));
    }
    if path.contains('\0') {
        return Err(tool_text_path(
            "tool-outbound-attachment-error-nul-byte",
            &path.escape_debug().to_string(),
        ));
    }
    if Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(tool_text_path(
            "tool-outbound-attachment-error-parent-dir",
            path,
        ));
    }
    if !security.is_path_allowed(path) {
        return Err(tool_text_path(
            "tool-outbound-attachment-error-policy",
            path,
        ));
    }

    let candidate = security.resolve_tool_path(path);
    let resolved = std::fs::canonicalize(&candidate)
        .map_err(|e| tool_text_path_err("tool-outbound-attachment-error-resolve", path, &e))?;
    if !security.is_resolved_path_readable(&resolved) {
        return Err(tool_text_path(
            "tool-outbound-attachment-error-outside-readable",
            path,
        ));
    }
    if security.is_runtime_config_path(&resolved) {
        return Err(tool_text_path(
            "tool-outbound-attachment-error-runtime-config",
            path,
        ));
    }

    let metadata = std::fs::metadata(&resolved)
        .map_err(|e| tool_text_path_err("tool-outbound-attachment-error-inspect", path, &e))?;
    if !metadata.is_file() {
        return Err(tool_text_path(
            "tool-outbound-attachment-error-not-regular",
            path,
        ));
    }
    if metadata.len() > max_bytes {
        return Err(too_large(path, metadata.len(), max_bytes));
    }

    let (Some(parent), Some(name)) = (resolved.parent(), resolved.file_name()) else {
        return Err(tool_text_path(
            "tool-outbound-attachment-error-no-file-name",
            path,
        ));
    };
    let file = open_absolute_dir_nofollow(parent)
        .and_then(|dir| open_file_nofollow(&dir, Path::new(name)))
        .map_err(|error| boundary_error(path, &error))?;

    let mut data = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|e| tool_text_path_err("tool-outbound-attachment-error-read", path, &e))?;
    let read_len = data.len() as u64;
    if read_len > max_bytes {
        return Err(too_large(path, read_len, max_bytes));
    }

    let file_name = name.to_string_lossy().into_owned();
    let mime_type = crate::file_upload::FileUploadTool::detect_mime(&data, &file_name);
    Ok(MediaAttachment {
        file_name,
        data,
        mime_type: Some(mime_type.to_string()),
        marker: None,
    })
}

fn too_large(path: &str, len: u64, max_bytes: u64) -> String {
    crate::i18n::get_required_tool_string_with_args(
        "tool-outbound-attachment-error-too-large",
        &[
            ("path", path),
            ("len", &len.to_string()),
            ("limit", &max_bytes.to_string()),
        ],
    )
}

fn boundary_error(path: &str, error: &FilesystemBoundaryError) -> String {
    match error {
        FilesystemBoundaryError::Denied { key, .. } if key.ends_with("-symlink") => {
            tool_text_path("tool-outbound-attachment-error-symlink", path)
        }
        FilesystemBoundaryError::Denied { .. } => {
            tool_text_path("tool-outbound-attachment-error-not-regular", path)
        }
        FilesystemBoundaryError::Io(e) => {
            tool_text_path_err("tool-outbound-attachment-error-open", path, e)
        }
    }
}

fn tool_text(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

fn tool_text_path(key: &str, path: &str) -> String {
    crate::i18n::get_required_tool_string_with_args(key, &[("path", path)])
}

fn tool_text_path_err(key: &str, path: &str, error: &std::io::Error) -> String {
    crate::i18n::get_required_tool_string_with_args(
        key,
        &[("path", path), ("err", &error.to_string())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn policy(workspace: PathBuf) -> SecurityPolicy {
        SecurityPolicy {
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        }
    }

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        let ws = ws.canonicalize().unwrap();
        (root, ws)
    }

    #[test]
    fn loads_relative_workspace_file_with_name_and_sniffed_mime() {
        let (_root, ws) = workspace();
        let pdf = b"%PDF-1.7\n%test\n";
        std::fs::create_dir(ws.join("out")).unwrap();
        std::fs::write(ws.join("out/report.bin"), pdf).unwrap();

        let attachment =
            read_workspace_attachment(&policy(ws.clone()), "out/report.bin", 1024).unwrap();
        assert_eq!(attachment.file_name, "report.bin");
        assert_eq!(attachment.data, pdf);
        assert_eq!(attachment.mime_type.as_deref(), Some("application/pdf"));
        assert!(attachment.marker.is_none());
    }

    #[test]
    fn falls_back_to_extension_mime_for_text() {
        let (_root, ws) = workspace();
        std::fs::write(ws.join("notes.csv"), "a,b\n1,2\n").unwrap();
        let attachment = read_workspace_attachment(&policy(ws), "notes.csv", 1024).unwrap();
        assert_eq!(attachment.mime_type.as_deref(), Some("text/csv"));
    }

    #[test]
    fn refuses_empty_nul_and_parent_components() {
        let (_root, ws) = workspace();
        let security = policy(ws);
        assert!(read_workspace_attachment(&security, "  ", 1024).is_err());
        let nul = read_workspace_attachment(&security, "a\0b", 1024).unwrap_err();
        assert!(nul.contains("NUL"), "{nul}");
        let parent = read_workspace_attachment(&security, "sub/../x.txt", 1024).unwrap_err();
        assert!(parent.contains("sub/../x.txt"), "{parent}");
    }

    #[test]
    fn refuses_a_file_over_the_cap() {
        let (_root, ws) = workspace();
        std::fs::write(ws.join("big.txt"), vec![b'x'; 11]).unwrap();
        let error = read_workspace_attachment(&policy(ws), "big.txt", 10).unwrap_err();
        assert!(error.contains("too large"), "{error}");
        assert!(error.contains("big.txt"), "{error}");
    }
}
