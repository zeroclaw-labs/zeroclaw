//! Client-side file attachment preparation.
//!
//! Validates files, detects MIME types, and builds `FileEntry` JSON
//! values for the `session/prompt` RPC call. Transport-aware: sends
//! `path` over Unix sockets, `data_b64` over WSS.
//!
//! Shared between Chat and ACP panes.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::client::Transport;

/// Per-file size limit matching the server's MAX_FILE_BYTES.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Where the attachment originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachmentSource {
    /// User picked a file via /attach or file explorer.
    File,
    /// Pasted from system clipboard (Ctrl+V).
    Clipboard,
}

/// A validated, display-ready attachment waiting to be sent.
#[derive(Debug, Clone)]
pub(crate) struct PendingAttachment {
    pub path: PathBuf,
    pub mime_type: String,
    pub filename: String,
    pub size_bytes: u64,
    pub source: AttachmentSource,
}

impl PendingAttachment {
    /// Validate a user-provided path and create a pending attachment.
    pub fn from_path(raw_path: &str) -> Result<Self> {
        let expanded = shellexpand::tilde(raw_path);
        let path = PathBuf::from(expanded.as_ref());
        if !path.is_absolute() {
            bail!("Path must be absolute: {}", path.display());
        }
        let meta = std::fs::metadata(&path)
            .with_context(|| format!("Cannot access: {}", path.display()))?;
        if !meta.is_file() {
            bail!("Not a regular file: {}", path.display());
        }
        if meta.len() > MAX_FILE_BYTES {
            bail!(
                "File too large: {} (limit {} MB)",
                format_size(meta.len()),
                MAX_FILE_BYTES / (1024 * 1024)
            );
        }
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload".to_string());
        let mime_type = mime_from_filename(&filename);
        Ok(Self {
            path,
            mime_type,
            filename,
            size_bytes: meta.len(),
            source: AttachmentSource::File,
        })
    }

    /// Batch-create from file explorer selections.
    pub fn from_explorer_paths(paths: &[PathBuf]) -> Result<Vec<Self>> {
        paths
            .iter()
            .map(|p| Self::from_path(&p.to_string_lossy()))
            .collect()
    }

    /// Human-readable label for display.
    pub fn label(&self) -> String {
        let size = format_size(self.size_bytes);
        format!("{} ({}, {})", self.filename, size, self.mime_type)
    }

    /// Build the `serde_json::Value` for a `FileEntry`.
    pub fn to_json(&self, transport: Transport) -> Result<serde_json::Value> {
        let source = match self.source {
            AttachmentSource::File => "file",
            AttachmentSource::Clipboard => "clipboard",
        };
        match transport {
            Transport::Unix => Ok(serde_json::json!({
                "path": self.path.to_string_lossy(),
                "filename": self.filename,
                "mime_type": self.mime_type,
                "source": source,
            })),
            Transport::Wss => {
                let bytes = std::fs::read(&self.path)
                    .with_context(|| format!("Reading {}", self.path.display()))?;
                let b64 = crate::mouse::base64_encode(&bytes);
                Ok(serde_json::json!({
                    "data_b64": b64,
                    "filename": self.filename,
                    "mime_type": self.mime_type,
                    "source": source,
                }))
            }
        }
    }
}

/// Build the JSON `attachments` array from pending attachments.
pub(crate) fn build_attachments_json(
    attachments: &[PendingAttachment],
    transport: Transport,
) -> Result<Vec<serde_json::Value>> {
    attachments.iter().map(|a| a.to_json(transport)).collect()
}

/// Detect MIME type from filename extension via `mime_guess`.
/// Falls back to `application/octet-stream` for unknown extensions.
pub(crate) fn mime_from_filename(filename: &str) -> String {
    mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string()
}

/// Human-readable file size.
pub(crate) fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_detection() {
        assert_eq!(mime_from_filename("photo.png"), "image/png");
        assert_eq!(mime_from_filename("doc.pdf"), "application/pdf");
        assert_eq!(mime_from_filename("data.csv"), "text/csv");
        assert_eq!(
            mime_from_filename("unknown.zzz"),
            "application/octet-stream"
        );
        assert_eq!(mime_from_filename("noext"), "application/octet-stream");
    }

    #[test]
    fn format_size_display() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn pending_attachment_label() {
        let att = PendingAttachment {
            path: PathBuf::from("/tmp/photo.png"),
            mime_type: "image/png".to_string(),
            filename: "photo.png".to_string(),
            size_bytes: 2048,
            source: AttachmentSource::File,
        };
        assert_eq!(att.label(), "photo.png (2.0 KB, image/png)");
    }

    #[test]
    fn to_json_unix_uses_path() {
        let att = PendingAttachment {
            path: PathBuf::from("/tmp/photo.png"),
            mime_type: "image/png".to_string(),
            filename: "photo.png".to_string(),
            size_bytes: 100,
            source: AttachmentSource::File,
        };
        // We can't actually call to_json without the file existing,
        // but for Unix mode it just serializes the path.
        let json = att.to_json(Transport::Unix).unwrap();
        assert!(json.get("path").is_some());
        assert!(json.get("data_b64").is_none());
        assert_eq!(json["filename"], "photo.png");
        assert_eq!(json["mime_type"], "image/png");
    }

    #[test]
    fn from_path_rejects_relative() {
        let result = PendingAttachment::from_path("relative/path.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("absolute"));
    }
}
