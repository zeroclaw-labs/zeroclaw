//! Lightweight config file watcher.
//!
//! Polls the config.toml mtime every few seconds and reloads the in-memory
//! config when a change is detected.  This ensures that config edits made
//! outside the gateway (CLI, TUI, direct file edits) are picked up without a
//! restart, and that connected dashboard clients are notified via SSE.

use super::AppState;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Spawn a background task that watches `config.toml` for external changes.
///
/// When the file's mtime changes, the task:
/// 1. Reloads `Config` from disk
/// 2. Updates the in-memory `AppState::config`
/// 3. Broadcasts a `config_updated` SSE event so dashboards auto-refresh
pub fn spawn_config_watcher(state: AppState, config_path: PathBuf) {
    tokio::spawn(async move {
        let mut last_mtime: Option<SystemTime> = file_mtime(&config_path);

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let current_mtime = file_mtime(&config_path);

            // Only reload when the mtime actually changed.
            if current_mtime == last_mtime {
                continue;
            }
            last_mtime = current_mtime;

            tracing::debug!("config.toml changed on disk — reloading");

            match Box::pin(crate::config::Config::load_or_init()).await {
                Ok(new_config) => {
                    *state.config.lock() = new_config;

                    let event = serde_json::json!({
                        "type": "config_updated",
                        "source": "file_watcher",
                    });
                    state.event_buffer.push(event.clone());
                    let _ = state.event_tx.send(event);
                }
                Err(e) => {
                    tracing::warn!("config.toml reload failed: {e}");
                }
            }
        }
    });
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn file_mtime_returns_none_for_missing_file() {
        let path = std::path::Path::new("/tmp/zeroclaw_nonexistent_config_test.toml");
        assert!(file_mtime(path).is_none());
    }

    #[test]
    fn file_mtime_returns_some_for_existing_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "test = true").unwrap();
        let mtime = file_mtime(tmp.path());
        assert!(mtime.is_some());
    }

    #[test]
    fn file_mtime_changes_on_write() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "v1").unwrap();
        let m1 = file_mtime(tmp.path());

        // Brief sleep to ensure distinct mtime on filesystems with 1s granularity
        std::thread::sleep(Duration::from_millis(1100));

        writeln!(tmp, "v2").unwrap();
        tmp.flush().unwrap();
        let m2 = file_mtime(tmp.path());

        assert_ne!(m1, m2);
    }
}
