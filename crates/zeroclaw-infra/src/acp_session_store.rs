use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use zeroclaw_api::provider::ConversationMessage;

pub struct AcpSessionStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

pub struct AcpSessionData {
    pub workspace_dir: String,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<ConversationMessage>,
}

impl AcpSessionStore {
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)
            .context("Failed to create sessions directory")?;
        let db_path = sessions_dir.join("acp-sessions.db");

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open ACP session DB: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;",
        )
        .context("Failed to configure ACP session DB pragmas")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS acp_sessions (
                session_id    TEXT PRIMARY KEY,
                workspace_dir TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                last_activity TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS acp_messages (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id    TEXT NOT NULL REFERENCES acp_sessions(session_id) ON DELETE CASCADE,
                message_json  TEXT NOT NULL,
                created_at    TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_acp_messages_session
                ON acp_messages(session_id, id);",
        )
        .context("Failed to create ACP session schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn new_creates_db_and_tables() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();

        let conn = store.conn.lock();
        let session_table: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='acp_sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(session_table, "acp_sessions");

        let msg_table: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='acp_messages'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_table, "acp_messages");
    }
}
