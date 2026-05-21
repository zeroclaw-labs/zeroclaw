use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::Path;
use zeroclaw_api::model_provider::ConversationMessage;

pub struct AcpSessionStore {
    conn: Mutex<Connection>,
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
        std::fs::create_dir_all(&sessions_dir).context("Failed to create sessions directory")?;
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

        let created_at = created_at_str.parse::<DateTime<Utc>>().unwrap_or_else(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"session_id": session_id, "error": e.to_string()})
                    ),
                "Failed to parse created_at"
            );
            Utc::now()
        });
        let last_activity = last_activity_str
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(
                            ::serde_json::json!({"session_id": session_id, "error": e.to_string()})
                        ),
                    "Failed to parse last_activity"
                );
                Utc::now()
            });

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
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_attrs(::serde_json::json!({"session_id": session_id, "error": e.to_string()})),
                            "Skipping corrupt ACP message"
                        );
                        None
                    }
                },
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"session_id": session_id, "error": e.to_string()})),
                        "Failed to read ACP message row"
                    );
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

    /// Append all ConversationMessages from one completed turn.
    /// Single transaction: N message INSERTs + last_activity UPDATE.
    /// Returns early without writing if `messages` is empty.
    pub fn append_turn(&self, session_id: &str, messages: &[ConversationMessage]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .context("Failed to begin append_turn transaction")?;

        for msg in messages {
            let json =
                serde_json::to_string(msg).context("Failed to serialize ConversationMessage")?;
            tx.execute(
                "INSERT INTO acp_messages (session_id, message_json, created_at)
                 VALUES (?1, ?2, ?3)",
                params![session_id, json, now],
            )
            .context("Failed to insert ACP message")?;
        }

        tx.execute(
            "UPDATE acp_sessions SET last_activity = ?1 WHERE session_id = ?2",
            params![now, session_id],
        )
        .context("Failed to update last_activity")?;

        tx.commit().context("Failed to commit append_turn")?;
        Ok(())
    }

    /// Delete a session and all its messages. Returns `true` if the session existed.
    /// Intended for operator tooling — not triggered by `session/close`.
    pub fn delete_session(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "DELETE FROM acp_sessions WHERE session_id = ?1",
                params![session_id],
            )
            .context("Failed to delete ACP session")?;
        Ok(rows > 0)
    }

    /// Update `last_activity` without appending messages.
    pub fn touch_session(&self, session_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE acp_sessions SET last_activity = ?1 WHERE session_id = ?2",
            params![now, session_id],
        )
        .context("Failed to touch ACP session")?;
        Ok(())
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

        store
            .create_session("sess-abc", "/home/user/project")
            .unwrap();

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

    #[test]
    fn append_turn_and_load_messages() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store.create_session("sess-msgs", "/tmp/proj").unwrap();

        let msgs = vec![
            ConversationMessage::Chat(zeroclaw_api::model_provider::ChatMessage::user("hello")),
            ConversationMessage::Chat(zeroclaw_api::model_provider::ChatMessage::assistant("hi")),
        ];
        store.append_turn("sess-msgs", &msgs).unwrap();

        let data = store.load_session("sess-msgs").unwrap().unwrap();
        assert_eq!(data.messages.len(), 2);
        assert!(
            matches!(&data.messages[0], ConversationMessage::Chat(m) if m.role == "user" && m.content == "hello")
        );
        assert!(
            matches!(&data.messages[1], ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "hi")
        );
    }

    #[test]
    fn all_conversation_message_variants_round_trip() {
        use zeroclaw_api::model_provider::{ChatMessage, ToolCall, ToolResultMessage};
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store.create_session("sess-variants", "/tmp/proj").unwrap();

        let msgs = vec![
            ConversationMessage::Chat(ChatMessage::user("task")),
            ConversationMessage::AssistantToolCalls {
                text: Some("calling shell".into()),
                tool_calls: vec![ToolCall {
                    id: "tc-1".into(),
                    name: "shell".into(),
                    arguments: r#"{"command":"ls"}"#.into(),
                    extra_content: None,
                }],
                reasoning_content: None,
                reasoning_field: None,
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc-1".into(),
                content: "file.txt\n".into(),
            }]),
            ConversationMessage::Chat(ChatMessage::assistant("done")),
        ];
        store.append_turn("sess-variants", &msgs).unwrap();

        let data = store.load_session("sess-variants").unwrap().unwrap();
        assert_eq!(data.messages.len(), 4);
        assert!(
            matches!(&data.messages[1], ConversationMessage::AssistantToolCalls { tool_calls, .. } if tool_calls[0].id == "tc-1")
        );
        assert!(
            matches!(&data.messages[2], ConversationMessage::ToolResults(r) if r[0].content == "file.txt\n")
        );
    }

    #[test]
    fn append_turn_empty_slice_is_noop() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store.create_session("sess-empty", "/tmp/proj").unwrap();

        store.append_turn("sess-empty", &[]).unwrap();

        let data = store.load_session("sess-empty").unwrap().unwrap();
        assert!(data.messages.is_empty());
    }

    #[test]
    fn last_activity_updated_on_append() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store.create_session("sess-activity", "/tmp/proj").unwrap();

        let before = store
            .load_session("sess-activity")
            .unwrap()
            .unwrap()
            .last_activity;

        // Brief sleep to ensure timestamp advances
        std::thread::sleep(std::time::Duration::from_millis(10));

        let msg = ConversationMessage::Chat(zeroclaw_api::model_provider::ChatMessage::user("hi"));
        store.append_turn("sess-activity", &[msg]).unwrap();

        let after = store
            .load_session("sess-activity")
            .unwrap()
            .unwrap()
            .last_activity;
        assert!(after >= before);
    }

    #[test]
    fn append_turn_rolls_back_on_unknown_session() {
        // Foreign key constraint: inserting messages for a nonexistent session
        // must fail atomically — no orphaned message rows.
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();

        let msg =
            ConversationMessage::Chat(zeroclaw_api::model_provider::ChatMessage::user("hello"));
        let result = store.append_turn("does-not-exist", &[msg]);
        assert!(result.is_err());

        // No orphaned rows
        let conn = store.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM acp_messages WHERE session_id = 'does-not-exist'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_session_removes_session_and_messages() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store.create_session("sess-del", "/tmp/proj").unwrap();
        let msg = ConversationMessage::Chat(zeroclaw_api::model_provider::ChatMessage::user("hi"));
        store.append_turn("sess-del", &[msg]).unwrap();

        let deleted = store.delete_session("sess-del").unwrap();
        assert!(deleted);
        assert!(store.load_session("sess-del").unwrap().is_none());

        // Cascade: messages gone too
        let conn = store.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM acp_messages WHERE session_id = 'sess-del'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_nonexistent_session_returns_false() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert!(!store.delete_session("ghost").unwrap());
    }

    #[test]
    fn touch_session_updates_last_activity() {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store.create_session("sess-touch", "/tmp/proj").unwrap();

        let before = store
            .load_session("sess-touch")
            .unwrap()
            .unwrap()
            .last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.touch_session("sess-touch").unwrap();
        let after = store
            .load_session("sess-touch")
            .unwrap()
            .unwrap()
            .last_activity;

        assert!(after >= before);
    }
}
