use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use zeroclaw_api::provider::ConversationMessage;

use serde_json;
use tracing;

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

    /// Record a new session. Call immediately after session/new succeeds.
    pub fn create_session(&self, session_id: &str, workspace_dir: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO acp_sessions (session_id, workspace_dir, created_at, last_activity)
             VALUES (?1, ?2, ?3, ?3)",
            params![session_id, workspace_dir, now],
        )
        .context("Failed to create ACP session")?;
        Ok(())
    }

    /// Load session metadata and full message history for restore.
    /// Returns `None` if the session_id is not found.
    pub fn load_session(&self, session_id: &str) -> Result<Option<AcpSessionData>> {
        let conn = self.conn.lock();

        let row = conn.query_row(
            "SELECT workspace_dir, created_at, last_activity
             FROM acp_sessions WHERE session_id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        );

        let (workspace_dir, created_at_str, last_activity_str) = match row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e).context("Failed to query ACP session"),
        };

        let created_at = created_at_str
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        let last_activity = last_activity_str
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());

        let mut stmt = conn
            .prepare(
                "SELECT message_json FROM acp_messages
                 WHERE session_id = ?1 ORDER BY id ASC",
            )
            .context("Failed to prepare message query")?;

        let messages: Vec<ConversationMessage> = stmt
            .query_map(params![session_id], |row| row.get::<_, String>(0))?
            .filter_map(|r| match r {
                Ok(json) => match serde_json::from_str::<ConversationMessage>(&json) {
                    Ok(msg) => Some(msg),
                    Err(e) => {
                        tracing::warn!("Skipping corrupt ACP message for session {session_id}: {e}");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read ACP message row for session {session_id}: {e}");
                    None
                }
            })
            .collect();

        Ok(Some(AcpSessionData {
            workspace_dir,
            created_at,
            last_activity,
            messages,
        }))
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

    #[test]
    fn create_and_load_session_metadata() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();

        store.create_session("sess-abc", "/home/user/project").unwrap();

        let data = store.load_session("sess-abc").unwrap().unwrap();
        assert_eq!(data.workspace_dir, "/home/user/project");
        assert!(data.messages.is_empty());
    }

    #[test]
    fn load_nonexistent_session_returns_none() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert!(store.load_session("nonexistent").unwrap().is_none());
    }
}
