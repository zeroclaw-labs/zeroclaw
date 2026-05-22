use chrono::Local;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::cleanup::cleanup_rotated_files;
use crate::config::RotationConfig;
use crate::error::RotationError;
use crate::rotate::{check_date_rotation, rotate_file, should_rotate_by_size};

/// Commands sent from the writer handle to the background task.
pub(crate) enum WriteCommand {
    Append {
        line: String,
    },
    /// Flush all pending appends and send ack, but keep the writer running.
    Flush {
        ack: tokio::sync::oneshot::Sender<()>,
    },
    Shutdown {
        ack: tokio::sync::oneshot::Sender<()>,
    },
}

/// State carried by the background task.
struct BackendState {
    path: PathBuf,
    config: RotationConfig,
}

impl BackendState {
    /// Handle a single write command.
    async fn handle(&mut self, cmd: WriteCommand) -> bool {
        match cmd {
            WriteCommand::Append { line } => {
                if let Err(e) = self.append_line(&line).await {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"error": e.to_string()})),
                        "Failed to append to rotated file"
                    );
                }
                true
            }
            WriteCommand::Flush { ack } => {
                // All prior Appends are already processed (FIFO channel).
                let _ = ack.send(());
                true
            }
            WriteCommand::Shutdown { ack } => {
                let _ = ack.send(());
                false
            }
        }
    }

    async fn append_line(&mut self, line: &str) -> crate::error::Result<()> {
        let now = chrono::Local::now();

        // 1. Date rotation: if active file is from a previous day, rotate it
        //    Named with mtime date (the data's actual date)
        if let Some(mtime_date) = check_date_rotation(&self.path, &now) {
            rotate_file(&self.path, mtime_date).await?;
            self.run_cleanup(&now).await;
        }

        // 2. Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RotationError::io(parent, e))?;
        }

        // 3. Open (or create) the active file in append mode and write
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).append(true);

        #[cfg(unix)]
        if let Some(mode) = self.config.file_permissions {
            #[allow(unused_imports)]
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }

        let mut file = options
            .open(&self.path)
            .await
            .map_err(|e| RotationError::io(&self.path, e))?;

        use tokio::io::AsyncWriteExt;
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        file.write_all(buf.as_bytes())
            .await
            .map_err(|e| RotationError::io(&self.path, e))?;

        // 4. sync_data() for durability
        if self.config.sync_on_write {
            file.sync_data()
                .await
                .map_err(|e| RotationError::io(&self.path, e))?;
        }

        drop(file);

        // 5. Re-apply permissions after write (in case OS changed them)
        #[cfg(unix)]
        if let Some(mode) = self.config.file_permissions {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(mode));
        }

        // 6. Size rotation: if the file now exceeds the size limit, rotate
        //    Named with current day (data was just written today)
        if should_rotate_by_size(&self.path, self.config.max_file_size_bytes) {
            rotate_file(&self.path, now.date_naive()).await?;
            self.run_cleanup(&now).await;
        }

        Ok(())
    }

    async fn run_cleanup(&self, now: &chrono::DateTime<Local>) {
        if let Err(e) = cleanup_rotated_files(&self.path, &self.config, now).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "File rotation cleanup failed"
            );
        }
    }
}

/// Run the background task that processes write commands.
pub(crate) async fn run_backend(
    rx: mpsc::Receiver<WriteCommand>,
    path: PathBuf,
    config: RotationConfig,
) {
    let mut state = BackendState { path, config };
    let mut rx = rx;

    loop {
        let cmd = rx.recv().await;
        match cmd {
            Some(WriteCommand::Shutdown { ack }) => {
                // Drain all remaining Append/Flush commands before shutting down,
                // so no in-flight writes are lost.
                while let Ok(pending) = rx.try_recv() {
                    state.handle(pending).await;
                }
                let _ = ack.send(());
                break;
            }
            Some(cmd) => {
                if !state.handle(cmd).await {
                    break;
                }
            }
            None => break,
        }
    }
}
