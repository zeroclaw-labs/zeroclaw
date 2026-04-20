use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::backend::{self, WriteCommand};
use crate::config::RotationConfig;
use crate::error::{Result, RotationError};

/// An async file writer with automatic size + date based rotation.
///
/// Writes are forwarded to a background tokio task via an mpsc channel,
/// so the caller is never blocked on disk I/O.
pub struct RotatingFileWriter {
    tx: mpsc::Sender<WriteCommand>,
    #[allow(dead_code)]
    handle: JoinHandle<()>,
}

impl RotatingFileWriter {
    /// Create a new rotating file writer.
    ///
    /// This spawns a background tokio task that handles all file I/O.
    /// The parent directory is created if it does not exist.
    pub async fn new(path: PathBuf, config: RotationConfig) -> Result<Self> {
        // Ensure parent directory exists up front
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RotationError::io(parent, e))?;
        }

        let (tx, rx) = mpsc::channel(4096);
        let cfg = config.clone();

        let handle = tokio::spawn(async move {
            backend::run_backend(rx, path, cfg).await;
        });

        Ok(Self { tx, handle })
    }

    /// Append a single line to the active file.
    ///
    /// The line is sent to the background task; this method returns once
    /// the command is buffered, not necessarily once it is on disk.
    pub async fn append(&self, line: String) -> Result<()> {
        self.tx
            .send(WriteCommand::Append { line })
            .await
            .map_err(|_| RotationError::ChannelClosed)
    }

    /// Gracefully shut down the writer.
    ///
    /// Sends a shutdown command and waits for the background task to
    /// flush all pending writes and exit.
    pub async fn shutdown(&self) -> Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(WriteCommand::Shutdown { ack: ack_tx })
            .await
            .map_err(|_| RotationError::ChannelClosed)?;
        ack_rx.await.map_err(|_| RotationError::ShutdownTimeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic_write_and_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.jsonl");

        let writer = RotatingFileWriter::new(
            path.clone(),
            RotationConfig {
                max_file_size_bytes: 1024 * 1024, // 1 MB
                sync_on_write: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        writer
            .append(r#"{"id":"1","event":"test"}"#.to_string())
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains(r#"{"id":"1","event":"test"}"#));
    }

    #[tokio::test]
    async fn size_rotation_creates_rotated_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app.log");

        let writer = RotatingFileWriter::new(
            path.clone(),
            RotationConfig {
                max_file_size_bytes: 50,
                max_age_days: 365,
                max_rotated_files: 100,
                sync_on_write: false,
                #[cfg(unix)]
                file_permissions: None,
            },
        )
        .await
        .unwrap();

        // Write enough lines to exceed 50 bytes
        for i in 0..20 {
            writer
                .append(format!("line-{:03} padding-to-exceed-threshold", i))
                .await
                .unwrap();
        }

        writer.shutdown().await.unwrap();

        // Verify at least one rotated file was created (the active file may or
        // may not exist depending on whether the very last write triggered a
        // size rotation).
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        let rotated: Vec<_> = entries
            .iter()
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                // Rotated files contain a date pattern (YYYY-MM-DD)
                name.contains("2026-") || name.contains("2025-")
            })
            .collect();
        assert!(
            !rotated.is_empty(),
            "Expected rotated files, found {:?}",
            entries
                .iter()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>()
        );
    }
}
