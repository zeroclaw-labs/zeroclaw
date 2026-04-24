//! iMessage channel — macOS native, polls chat.db + sends via AppleScript.
//!
//! Reads incoming messages from `~/Library/Messages/chat.db` (SQLite) and
//! sends replies via `Messages.app` AppleScript. Requires Full Disk Access
//! for chat.db read and Automation permission for Messages.app.

use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::IMessageConfig;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Cursor file for persisting last-seen ROWID across daemon restarts.
const CURSOR_FILE: &str = "imessage_cursor";

/// iMessage channel — reads from chat.db, sends via AppleScript.
pub struct IMessageChannel {
    config: IMessageConfig,
    db_path: PathBuf,
    last_rowid: Arc<AtomicI64>,
    poll_interval: Duration,
    cursor_path: PathBuf,
}

impl IMessageChannel {
    pub fn new(config: IMessageConfig) -> Self {
        let db_path = config
            .db_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(default_chat_db_path);

        let poll_interval = Duration::from_secs(config.poll_interval_secs.max(1));

        let cursor_path = augusta_config_dir().join(CURSOR_FILE);

        let last_rowid = load_cursor(&cursor_path);

        Self {
            config,
            db_path,
            last_rowid: Arc::new(AtomicI64::new(last_rowid)),
            poll_interval,
            cursor_path,
        }
    }

    /// Check if a sender is in the allowed contacts list.
    fn is_allowed(&self, sender: &str) -> bool {
        if self.config.allowed_contacts.is_empty() {
            return false;
        }
        if self.config.allowed_contacts.iter().any(|c| c == "*") {
            return true;
        }
        // Normalize: strip whitespace, compare case-insensitive for emails
        let normalized = sender.trim();
        self.config.allowed_contacts.iter().any(|c| {
            let c = c.trim();
            if c.contains('@') || normalized.contains('@') {
                c.eq_ignore_ascii_case(normalized)
            } else {
                // Phone: strip non-digit chars for comparison
                let c_digits: String = c.chars().filter(|ch| ch.is_ascii_digit()).collect();
                let s_digits: String = normalized
                    .chars()
                    .filter(|ch| ch.is_ascii_digit())
                    .collect();
                // Match on last 10 digits (handles +1 prefix vs no prefix)
                let c_tail = if c_digits.len() >= 10 {
                    &c_digits[c_digits.len() - 10..]
                } else {
                    &c_digits
                };
                let s_tail = if s_digits.len() >= 10 {
                    &s_digits[s_digits.len() - 10..]
                } else {
                    &s_digits
                };
                c_tail == s_tail
            }
        })
    }

    /// Save cursor to disk.
    fn save_cursor(&self) {
        let rowid = self.last_rowid.load(Ordering::Relaxed);
        if let Err(e) = std::fs::write(&self.cursor_path, rowid.to_string()) {
            warn!("Failed to save iMessage cursor: {e}");
        }
    }

    /// Execute an AppleScript to send an iMessage. macOS only.
    #[cfg(target_os = "macos")]
    async fn run_applescript(script: String, recipient: &str) -> anyhow::Result<()> {
        let result =
            tokio::task::spawn_blocking(move || lightwave_macos::applescript::execute(&script))
                .await??;
        debug!("iMessage sent to {recipient}: {result}");
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    async fn run_applescript(_script: String, _recipient: &str) -> anyhow::Result<()> {
        anyhow::bail!("iMessage send is only supported on macOS")
    }
}

#[async_trait]
impl Channel for IMessageChannel {
    fn name(&self) -> &str {
        "imessage"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let recipient = &message.recipient;
        let content = &message.content;

        // Escape for AppleScript string literal
        let escaped = content
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");

        let service = "iMessage";

        let script = format!(
            r#"tell application "Messages"
    set targetService to 1st account whose service type = {service}
    set targetBuddy to participant "{recipient}" of targetService
    send "{escaped}" to targetBuddy
end tell"#,
            service = service,
            recipient = recipient,
            escaped = escaped,
        );

        Self::run_applescript(script, recipient).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        let last_rowid = Arc::clone(&self.last_rowid);
        let poll_interval = self.poll_interval;
        let allowed_contacts = self.config.allowed_contacts.clone();

        info!("iMessage listener started");
        info!("  Database: {}", db_path.display());
        info!("  Poll interval: {}s", poll_interval.as_secs());
        info!("  Allowed contacts: {}", allowed_contacts.len());
        info!("  Cursor: ROWID > {}", last_rowid.load(Ordering::Relaxed));

        loop {
            let current_rowid = last_rowid.load(Ordering::Relaxed);

            match poll_new_messages(&db_path, current_rowid).await {
                Ok(messages) => {
                    let mut max_rowid = current_rowid;

                    for (rowid, sender, text, date) in messages {
                        if rowid > max_rowid {
                            max_rowid = rowid;
                        }

                        if !self.is_allowed(&sender) {
                            debug!("iMessage from non-allowed sender: {sender}");
                            continue;
                        }

                        let msg = ChannelMessage {
                            id: Uuid::new_v4().to_string(),
                            sender: sender.clone(),
                            reply_target: sender,
                            content: text,
                            channel: "imessage".to_string(),
                            timestamp: date,
                            thread_ts: None,
                            metadata: HashMap::new(),
                        };

                        if tx.send(msg).await.is_err() {
                            info!("iMessage listener: receiver dropped, stopping");
                            return Ok(());
                        }
                    }

                    if max_rowid > current_rowid {
                        last_rowid.store(max_rowid, Ordering::Relaxed);
                        self.save_cursor();
                    }
                }
                Err(e) => {
                    error!("iMessage poll error: {e}");
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Verify chat.db is readable (Full Disk Access check)
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            match rusqlite::Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) {
                Ok(conn) => {
                    // Try a minimal query
                    conn.query_row("SELECT MAX(ROWID) FROM message", [], |_| Ok(()))
                        .is_ok()
                }
                Err(e) => {
                    warn!("iMessage health check failed (Full Disk Access required?): {e}");
                    false
                }
            }
        })
        .await
        .unwrap_or(false)
    }
}

/// Poll chat.db for messages with ROWID > last_seen where is_from_me = 0.
/// Returns Vec<(rowid, sender_id, text, timestamp)>.
async fn poll_new_messages(
    db_path: &Path,
    last_rowid: i64,
) -> anyhow::Result<Vec<(i64, String, String, u64)>> {
    let db_path = db_path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        // chat.db schema: message table has ROWID, text, handle_id, is_from_me, date
        // handle table maps handle_id → phone/email (the "id" column)
        // date is in Apple Core Data epoch (nanoseconds since 2001-01-01)
        let mut stmt = conn.prepare(
            "SELECT m.ROWID, h.id, m.text, m.date
             FROM message m
             JOIN handle h ON m.handle_id = h.ROWID
             WHERE m.ROWID > ?1 AND m.is_from_me = 0 AND m.text IS NOT NULL
             ORDER BY m.ROWID ASC",
        )?;

        let rows = stmt.query_map([last_rowid], |row| {
            let rowid: i64 = row.get(0)?;
            let sender: String = row.get(1)?;
            let text: String = row.get(2)?;
            let apple_date: i64 = row.get(3)?;
            // Convert Apple Core Data date (nanoseconds since 2001-01-01) to Unix epoch
            // 978307200 = seconds between 1970-01-01 and 2001-01-01
            #[allow(clippy::cast_sign_loss)]
            let unix_ts = (apple_date / 1_000_000_000 + 978_307_200) as u64;
            Ok((rowid, sender, text, unix_ts))
        })?;

        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(r) => results.push(r),
                Err(e) => warn!("Failed to parse iMessage row: {e}"),
            }
        }

        Ok(results)
    })
    .await?
}

/// Default macOS Messages chat.db location.
fn default_chat_db_path() -> PathBuf {
    home_dir().join("Library/Messages/chat.db")
}

/// Augusta config directory (~/.config/augusta/).
fn augusta_config_dir() -> PathBuf {
    home_dir().join(".config/augusta")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Load cursor from disk, defaulting to 0.
fn load_cursor(path: &PathBuf) -> i64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imessage_channel_name() {
        let config = IMessageConfig {
            allowed_contacts: vec![],
            poll_interval_secs: 2,
            db_path: Some("/tmp/test_chat.db".into()),
        };
        let ch = IMessageChannel::new(config);
        assert_eq!(ch.name(), "imessage");
    }

    #[test]
    fn allowed_contact_exact_match() {
        let config = IMessageConfig {
            allowed_contacts: vec!["+13235366769".into()],
            poll_interval_secs: 2,
            db_path: None,
        };
        let ch = IMessageChannel::new(config);
        assert!(ch.is_allowed("+13235366769"));
        assert!(!ch.is_allowed("+10000000000"));
    }

    #[test]
    fn allowed_contact_wildcard() {
        let config = IMessageConfig {
            allowed_contacts: vec!["*".into()],
            poll_interval_secs: 2,
            db_path: None,
        };
        let ch = IMessageChannel::new(config);
        assert!(ch.is_allowed("+10000000000"));
        assert!(ch.is_allowed("anyone@example.com"));
    }

    #[test]
    fn allowed_contact_empty_denies_all() {
        let config = IMessageConfig {
            allowed_contacts: vec![],
            poll_interval_secs: 2,
            db_path: None,
        };
        let ch = IMessageChannel::new(config);
        assert!(!ch.is_allowed("+10000000000"));
    }

    #[test]
    fn allowed_contact_email_case_insensitive() {
        let config = IMessageConfig {
            allowed_contacts: vec!["User@iCloud.com".into()],
            poll_interval_secs: 2,
            db_path: None,
        };
        let ch = IMessageChannel::new(config);
        assert!(ch.is_allowed("user@icloud.com"));
        assert!(ch.is_allowed("USER@ICLOUD.COM"));
    }

    #[test]
    fn allowed_contact_phone_normalization() {
        let config = IMessageConfig {
            allowed_contacts: vec!["+1 (323) 536-6769".into()],
            poll_interval_secs: 2,
            db_path: None,
        };
        let ch = IMessageChannel::new(config);
        assert!(ch.is_allowed("+13235366769"));
        assert!(ch.is_allowed("3235366769"));
    }

    #[test]
    fn default_chat_db_path_is_messages() {
        let path = default_chat_db_path();
        assert!(path.to_string_lossy().contains("Library/Messages/chat.db"));
    }

    #[test]
    fn cursor_load_missing_file() {
        let path = PathBuf::from("/tmp/nonexistent_cursor_test_12345");
        assert_eq!(load_cursor(&path), 0);
    }

    #[test]
    fn cursor_roundtrip() {
        let path = std::env::temp_dir().join("augusta_cursor_test");
        std::fs::write(&path, "42").unwrap();
        assert_eq!(load_cursor(&path), 42);
        let _ = std::fs::remove_file(&path);
    }
}
