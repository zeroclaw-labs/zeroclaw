//! SQLite-backed session persistence with FTS5 search.

use crate::session_backend::{
    ChannelConversationRecord, ConditionalSessionWrite, SessionBackend, SessionContext,
    SessionMetadata, SessionMutation, SessionQuery, SessionState,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;
use zeroclaw_api::model_provider::ChatMessage;

/// SQLite-backed session store with FTS5 and WAL mode.
pub struct SqliteSessionBackend {
    conn: Mutex<Connection>,
}

fn ensure_column(conn: &Connection, column: &str, ddl: &str) -> Result<()> {
    let present: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') WHERE name = ?1",
            params![column],
            |row| row.get(0),
        )
        .with_context(|| format!("Failed to probe session_metadata.{column}"))?;
    if !present {
        conn.execute(ddl, [])
            .with_context(|| format!("Failed to add session_metadata.{column}"))?;
    }
    Ok(())
}

impl SqliteSessionBackend {
    /// Open or create the sessions database.
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).context("Failed to create sessions directory")?;
        let db_path = sessions_dir.join("sessions.db");

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open session DB: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 4194304;
             PRAGMA busy_timeout = 5000;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_key TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                created_at  TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_key ON sessions(session_key);
             CREATE INDEX IF NOT EXISTS idx_sessions_key_id ON sessions(session_key, id);

             CREATE TABLE IF NOT EXISTS session_metadata (
                session_key  TEXT PRIMARY KEY,
                created_at   TEXT NOT NULL,
                last_activity TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0,
                name         TEXT
             );

             CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(
                session_key, content, content=sessions, content_rowid=id
             );

             CREATE TRIGGER IF NOT EXISTS sessions_ai AFTER INSERT ON sessions BEGIN
                INSERT INTO sessions_fts(rowid, session_key, content)
                VALUES (new.id, new.session_key, new.content);
             END;
             CREATE TRIGGER IF NOT EXISTS sessions_ad AFTER DELETE ON sessions BEGIN
                INSERT INTO sessions_fts(sessions_fts, rowid, session_key, content)
                VALUES ('delete', old.id, old.session_key, old.content);
             END;
             CREATE TRIGGER IF NOT EXISTS sessions_au AFTER UPDATE ON sessions BEGIN
                INSERT INTO sessions_fts(sessions_fts, rowid, session_key, content)
                VALUES ('delete', old.id, old.session_key, old.content);
                INSERT INTO sessions_fts(rowid, session_key, content)
                VALUES (new.id, new.session_key, new.content);
             END;",
        )
        .context("Failed to initialize session schema")?;

        for (column, ddl) in [
            ("name", "ALTER TABLE session_metadata ADD COLUMN name TEXT"),
            (
                "state",
                "ALTER TABLE session_metadata ADD COLUMN state TEXT NOT NULL DEFAULT 'idle'",
            ),
            (
                "turn_id",
                "ALTER TABLE session_metadata ADD COLUMN turn_id TEXT",
            ),
            (
                "turn_started_at",
                "ALTER TABLE session_metadata ADD COLUMN turn_started_at TEXT",
            ),
            (
                "agent_alias",
                "ALTER TABLE session_metadata ADD COLUMN agent_alias TEXT",
            ),
            (
                "channel_id",
                "ALTER TABLE session_metadata ADD COLUMN channel_id TEXT",
            ),
            (
                "room_id",
                "ALTER TABLE session_metadata ADD COLUMN room_id TEXT",
            ),
            (
                "sender_id",
                "ALTER TABLE session_metadata ADD COLUMN sender_id TEXT",
            ),
            (
                "conversation_id",
                "ALTER TABLE session_metadata ADD COLUMN conversation_id TEXT",
            ),
        ] {
            ensure_column(&conn, column, ddl)?;
        }
        for ddl in [
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_agent_alias ON session_metadata(agent_alias)",
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_channel_id ON session_metadata(channel_id)",
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_room_id ON session_metadata(room_id)",
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_sender_id ON session_metadata(sender_id)",
        ] {
            conn.execute(ddl, [])?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Migrate JSONL session files into SQLite. Renames migrated files to
    /// `.jsonl.migrated`. Each session is imported in ONE `IMMEDIATE`
    /// transaction: all messages + one metadata upsert (carrying the JSONL
    /// sidecar's `conversation_id` if present) commit together, so a partial
    /// failure never leaves half-imported data. The source file is renamed
    /// ONLY after commit; a rename failure returns an error (not silently
    /// increments `migrated`). Idempotent: a session already present in the
    /// DB is skipped so a retried migration does not duplicate messages.
    pub fn migrate_from_jsonl(&self, workspace_dir: &Path) -> Result<usize> {
        let source = crate::session_store::SessionStore::new(workspace_dir)?;
        let mut migrated = 0;
        for key in source.list_sessions() {
            let imported = source.with_locked_conversation(&key, |load, path| {
                let mut conn = self.conn.lock();
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(std::io::Error::other)?;
                let already: bool = tx
                    .query_row(
                        "SELECT COUNT(*) > 0 FROM session_metadata WHERE session_key = ?1",
                        params![key],
                        |row| row.get(0),
                    )
                    .map_err(std::io::Error::other)?;
                if already {
                    return Ok(false);
                }
                let Some(record) = load()? else {
                    return Ok(false);
                };
                let now = Utc::now().to_rfc3339();
                tx.execute(
                    "INSERT INTO session_metadata
                        (session_key, created_at, last_activity, message_count, conversation_id)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        key,
                        now,
                        now,
                        record.history.len() as i64,
                        record.conversation_id
                    ],
                )
                .map_err(std::io::Error::other)?;
                for message in record.history {
                    tx.execute(
                        "INSERT INTO sessions (session_key, role, content, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![key, message.role, message.content, now],
                    )
                    .map_err(std::io::Error::other)?;
                }
                tx.commit().map_err(std::io::Error::other)?;
                drop(conn);
                std::fs::rename(path, path.with_extension("jsonl.migrated"))?;
                Ok(true)
            })?;
            if imported {
                migrated += 1;
            }
        }
        Ok(migrated)
    }

    fn load_messages(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT role, content FROM sessions WHERE session_key = ?1 ORDER BY id ASC")
            .map_err(std::io::Error::other)?;
        let rows = stmt
            .query_map(params![session_key], |row| {
                Ok(ChatMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                })
            })
            .map_err(std::io::Error::other)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(std::io::Error::other)
    }
}

impl SessionBackend for SqliteSessionBackend {
    fn resolve_or_create_conversation_id(&self, key: &str) -> std::io::Result<String> {
        Ok(self.open_conversation(key)?.conversation_id)
    }
    fn clear_and_rotate_conversation(&self, key: &str) -> std::io::Result<String> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;
        let now = Utc::now().to_rfc3339();
        tx.execute("INSERT INTO session_metadata(session_key,created_at,last_activity,message_count) VALUES(?1,?2,?3,0) ON CONFLICT(session_key) DO NOTHING", params![key,now,now]).map_err(std::io::Error::other)?;
        tx.execute("DELETE FROM sessions WHERE session_key=?1", params![key])
            .map_err(std::io::Error::other)?;
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute("UPDATE session_metadata SET message_count=0,last_activity=?1,conversation_id=?2 WHERE session_key=?3", params![now,id,key]).map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(id)
    }
    fn append_if_conversation_matches(
        &self,
        key: &str,
        id: &str,
        message: &ChatMessage,
    ) -> std::io::Result<ConditionalSessionWrite> {
        self.mutate_conversation_if_current(key, id, SessionMutation::Append(message))
    }
    fn remove_last_if_conversation_matches(
        &self,
        key: &str,
        expected: &str,
    ) -> std::io::Result<ConditionalSessionWrite> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;
        let current: Option<Option<String>> = tx
            .query_row(
                "SELECT conversation_id FROM session_metadata WHERE session_key=?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(std::io::Error::other)?;
        let Some(Some(current)) = current else {
            return Ok(ConditionalSessionWrite::Deleted);
        };
        let valid = |id: &str| uuid::Uuid::parse_str(id).is_ok_and(|u| u.get_version_num() == 4);
        if !valid(&current) || !valid(expected) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "conversation identity is not UUID v4",
            ));
        }
        if current != expected {
            return Ok(ConditionalSessionWrite::Stale);
        }
        let last: Option<i64> = tx
            .query_row(
                "SELECT id FROM sessions WHERE session_key=?1 ORDER BY id DESC LIMIT 1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(std::io::Error::other)?;
        if let Some(id) = last {
            tx.execute("DELETE FROM sessions WHERE id=?1", params![id])
                .map_err(std::io::Error::other)?;
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE session_key=?1",
                params![key],
                |row| row.get(0),
            )
            .map_err(std::io::Error::other)?;
        tx.execute(
            "UPDATE session_metadata SET message_count=?1 WHERE session_key=?2",
            params![count, key],
        )
        .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(ConditionalSessionWrite::Applied)
    }

    fn update_last_if_conversation_matches(
        &self,
        key: &str,
        id: &str,
        message: &ChatMessage,
    ) -> std::io::Result<ConditionalSessionWrite> {
        self.mutate_conversation_if_current(key, id, SessionMutation::UpdateLast(message))
    }

    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        self.load_messages(session_key).unwrap_or_default()
    }

    fn load_fallible(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        self.load_messages(session_key)
    }

    fn load_with_timestamps(
        &self,
        session_key: &str,
    ) -> Vec<crate::session_backend::TimestampedMessage> {
        use crate::session_backend::TimestampedMessage;
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT role, content, created_at FROM sessions WHERE session_key = ?1 ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map(params![session_key], |row| {
            let role: String = row.get(0)?;
            let content: String = row.get(1)?;
            let created_at_raw: Option<String> = row.get(2).ok();
            let created_at = created_at_raw
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            Ok(TimestampedMessage {
                message: ChatMessage { role, content },
                created_at,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO sessions (session_key, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_key, message.role, message.content, now],
        )
        .map_err(std::io::Error::other)?;

        // Upsert metadata
        conn.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(session_key) DO UPDATE SET
                last_activity = excluded.last_activity,
                message_count = message_count + 1",
            params![session_key, now, now],
        )
        .map_err(std::io::Error::other)?;

        Ok(())
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let conn = self.conn.lock();

        let last_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM sessions WHERE session_key = ?1 ORDER BY id DESC LIMIT 1",
                params![session_key],
                |row| row.get(0),
            )
            .ok();

        let Some(id) = last_id else {
            return Ok(false);
        };

        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(std::io::Error::other)?;

        // Update metadata count
        conn.execute(
            "UPDATE session_metadata SET message_count = MAX(0, message_count - 1)
             WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    /// Efficiently update the last message in-place (single UPDATE instead of
    /// DELETE + INSERT). Used for incremental persistence during streaming.
    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        let conn = self.conn.lock();

        let last_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM sessions WHERE session_key = ?1 ORDER BY id DESC LIMIT 1",
                params![session_key],
                |row| row.get(0),
            )
            .ok();

        let Some(id) = last_id else {
            return Ok(false);
        };

        conn.execute(
            "UPDATE sessions SET role = ?1, content = ?2 WHERE id = ?3",
            params![message.role, message.content, id],
        )
        .map_err(std::io::Error::other)?;

        // NOTE: FTS index becomes stale here (no UPDATE trigger, only
        // INSERT/DELETE triggers). This is acceptable — update_last is
        // used for transient streaming snapshots. The final content will
        // be correct in the sessions table for load().

        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE session_metadata SET last_activity = ?1 WHERE session_key = ?2",
            params![now, session_key],
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    fn list_sessions(&self) -> Vec<String> {
        let conn = self.conn.lock();
        let mut stmt = match conn
            .prepare("SELECT session_key FROM session_metadata ORDER BY last_activity DESC")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| row.get(0)) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, conversation_id
             FROM session_metadata ORDER BY last_activity DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let created_str: String = row.get(1)?;
            let activity_str: String = row.get(2)?;
            let count: i64 = row.get(3)?;
            let name: Option<String> = row.get(4)?;
            let agent_alias: Option<String> = row.get(5)?;
            let channel_id: Option<String> = row.get(6)?;
            let room_id: Option<String> = row.get(7)?;
            let sender_id: Option<String> = row.get(8)?;
            let conversation_id: Option<String> = row.get(9)?;

            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
                conversation_id,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let conn = self.conn.lock();
        let cutoff = (Utc::now() - Duration::hours(i64::from(ttl_hours))).to_rfc3339();

        // Find stale sessions
        let stale_keys: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT session_key FROM session_metadata WHERE last_activity < ?1")
                .map_err(std::io::Error::other)?;
            let rows = stmt
                .query_map(params![cutoff], |row| row.get(0))
                .map_err(std::io::Error::other)?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let count = stale_keys.len();
        for key in &stale_keys {
            let _ = conn.execute("DELETE FROM sessions WHERE session_key = ?1", params![key]);
            let _ = conn.execute(
                "DELETE FROM session_metadata WHERE session_key = ?1",
                params![key],
            );
        }

        Ok(count)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let conn = self.conn.lock();

        conn.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        let count = conn.changes() as usize;

        if count > 0 {
            conn.execute(
                "UPDATE session_metadata SET message_count = 0, last_activity = ?1 WHERE session_key = ?2",
                params![Utc::now().to_rfc3339(), session_key],
            )
            .map_err(std::io::Error::other)?;
        }

        Ok(count)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;

        // Check if session exists. A real DB error MUST propagate (not be
        // misreported as "does not exist" via unwrap_or(false)).
        let exists: bool = tx
            .query_row(
                "SELECT COUNT(*) > 0 FROM session_metadata WHERE session_key = ?1",
                params![session_key],
                |row| row.get(0),
            )
            .map_err(std::io::Error::other)?;

        if !exists {
            return Ok(false);
        }

        // Delete messages (FTS5 trigger handles sessions_fts cleanup) and
        // metadata together, in one transaction, so a failure on the second
        // statement rolls back the first instead of leaving orphaned rows.
        tx.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        tx.execute(
            "DELETE FROM session_metadata WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        tx.commit().map_err(std::io::Error::other)?;
        Ok(true)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE session_metadata SET agent_alias = NULL WHERE agent_alias = ?1",
                params![agent_alias],
            )
            .map_err(std::io::Error::other)?;
        Ok(rows)
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE session_metadata SET agent_alias = ?2 WHERE agent_alias = ?1",
                params![from, to],
            )
            .map_err(std::io::Error::other)?;
        Ok(rows)
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        // Mirror the `WHERE agent_alias = ?1` predicate `rename_agent_attribution`
        // re-points, so the residue probe matches exactly what a resume moves.
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_metadata WHERE agent_alias = ?1",
                params![agent_alias],
                |row| row.get(0),
            )
            .map_err(std::io::Error::other)?;
        Ok(count.max(0) as usize)
    }

    fn session_exists(&self, session_key: &str) -> bool {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT 1 FROM session_metadata WHERE session_key = ?1 LIMIT 1",
            params![session_key],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn current_conversation_id(&self, session_key: &str) -> std::io::Result<Option<String>> {
        // Read the column directly instead of routing through get_session_metadata,
        // so is_current / existing_record_for_test do not depend on every metadata
        // field being populated and remain efficient on the JSONL parity path.
        let conn = self.conn.lock();
        let id: Option<Option<String>> = conn
            .query_row(
                "SELECT conversation_id FROM session_metadata WHERE session_key = ?1",
                params![session_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(std::io::Error::other)?;
        Ok(id.flatten())
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let name_val = if name.is_empty() { None } else { Some(name) };
        conn.execute(
            "UPDATE session_metadata SET name = ?1 WHERE session_key = ?2",
            params![name_val, session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT name FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| row.get(0),
        )
        .map_err(std::io::Error::other)
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, conversation_id
             FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| {
                let key: String = row.get(0)?;
                let created_str: String = row.get(1)?;
                let activity_str: String = row.get(2)?;
                let count: i64 = row.get(3)?;
                let name: Option<String> = row.get(4)?;
                let agent_alias: Option<String> = row.get(5)?;
                let channel_id: Option<String> = row.get(6)?;
                let room_id: Option<String> = row.get(7)?;
                let sender_id: Option<String> = row.get(8)?;
                let conversation_id: Option<String> = row.get(9)?;

                let created = DateTime::parse_from_rfc3339(&created_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let activity = DateTime::parse_from_rfc3339(&activity_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                Ok(SessionMetadata {
                    key,
                    name,
                    created_at: created,
                    last_activity: activity,
                    message_count: count as usize,
                    agent_alias,
                    channel_id,
                    room_id,
                    sender_id,
                    conversation_id,
                })
            },
        )
        .ok()
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let started_at = if state == "running" {
            Some(now.as_str())
        } else {
            None
        };
        conn.execute(
            "UPDATE session_metadata SET state = ?1, turn_id = ?2, turn_started_at = ?3
             WHERE session_key = ?4",
            params![state, turn_id, started_at, session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT state, turn_id, turn_started_at FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| {
                let state: String = row.get(0)?;
                let turn_id: Option<String> = row.get(1)?;
                let started_str: Option<String> = row.get(2)?;
                let turn_started_at = started_str.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                });
                Ok(SessionState {
                    state,
                    turn_id,
                    turn_started_at,
                })
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(std::io::Error::other(other)),
        })
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, conversation_id
             FROM session_metadata WHERE state = 'running' ORDER BY turn_started_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let created_str: String = row.get(1)?;
            let activity_str: String = row.get(2)?;
            let count: i64 = row.get(3)?;
            let name: Option<String> = row.get(4)?;
            let agent_alias: Option<String> = row.get(5)?;
            let channel_id: Option<String> = row.get(6)?;
            let room_id: Option<String> = row.get(7)?;
            let sender_id: Option<String> = row.get(8)?;
            let conversation_id: Option<String> = row.get(9)?;
            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
                conversation_id,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let conn = self.conn.lock();
        #[allow(clippy::cast_possible_wrap)]
        let cutoff = (Utc::now() - chrono::Duration::seconds(threshold_secs as i64)).to_rfc3339();
        let mut stmt = match conn.prepare(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, conversation_id
             FROM session_metadata
             WHERE state = 'running' AND turn_started_at < ?1
             ORDER BY turn_started_at ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map(params![cutoff], |row| {
            let key: String = row.get(0)?;
            let created_str: String = row.get(1)?;
            let activity_str: String = row.get(2)?;
            let count: i64 = row.get(3)?;
            let name: Option<String> = row.get(4)?;
            let agent_alias: Option<String> = row.get(5)?;
            let channel_id: Option<String> = row.get(6)?;
            let room_id: Option<String> = row.get(7)?;
            let sender_id: Option<String> = row.get(8)?;
            let conversation_id: Option<String> = row.get(9)?;
            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
                conversation_id,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Some(keyword) = &query.keyword else {
            return self.list_sessions_with_metadata();
        };

        let conn = self.conn.lock();
        #[allow(clippy::cast_possible_wrap)]
        let limit = query.limit.unwrap_or(50) as i64;

        // FTS5 search
        let mut stmt = match conn.prepare(
            "SELECT DISTINCT f.session_key
             FROM sessions_fts f
             WHERE sessions_fts MATCH ?1
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        // Quote each word for FTS5
        let fts_query: String = keyword
            .split_whitespace()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" OR ");

        let keys: Vec<String> = match stmt.query_map(params![fts_query, limit], |row| row.get(0)) {
            Ok(r) => r.filter_map(|r| r.ok()).collect(),
            Err(_) => return Vec::new(),
        };

        // Look up metadata for matched sessions
        keys.iter()
            .filter_map(|key| {
                conn.query_row(
                    "SELECT created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, conversation_id FROM session_metadata WHERE session_key = ?1",
                    params![key],
                    |row| {
                        let created_str: String = row.get(0)?;
                        let activity_str: String = row.get(1)?;
                        let count: i64 = row.get(2)?;
                        let name: Option<String> = row.get(3)?;
                        let agent_alias: Option<String> = row.get(4)?;
                        let channel_id: Option<String> = row.get(5)?;
                        let room_id: Option<String> = row.get(6)?;
                        let sender_id: Option<String> = row.get(7)?;
                        let conversation_id: Option<String> = row.get(8)?;
                        Ok(SessionMetadata {
                            key: key.clone(),
                            name,
                            created_at: DateTime::parse_from_rfc3339(&created_str)
                                .map(|dt| dt.with_timezone(&Utc))
                                .unwrap_or_else(|_| Utc::now()),
                            last_activity: DateTime::parse_from_rfc3339(&activity_str)
                                .map(|dt| dt.with_timezone(&Utc))
                                .unwrap_or_else(|_| Utc::now()),
                            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                            message_count: count as usize,
                            agent_alias,
                            channel_id,
                            room_id,
                            sender_id,
                            conversation_id,
                        })
                    },
                )
                .ok()
            })
            .collect()
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let alias_val = if agent_alias.is_empty() {
            None
        } else {
            Some(agent_alias)
        };
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count, agent_alias)
             VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT(session_key) DO UPDATE SET agent_alias = excluded.agent_alias",
            params![session_key, now, now, alias_val],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT agent_alias FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| row.get(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(std::io::Error::other(other)),
        })
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let conn = self.conn.lock();
        fn normalize(v: Option<&str>) -> Option<&str> {
            v.map(str::trim).filter(|s| !s.is_empty())
        }
        let channel_id = normalize(context.channel_id);
        let room_id = normalize(context.room_id);
        let sender_id = normalize(context.sender_id);
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO session_metadata
                (session_key, created_at, last_activity, message_count, channel_id, room_id, sender_id)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)
             ON CONFLICT(session_key) DO UPDATE SET
                channel_id = COALESCE(excluded.channel_id, session_metadata.channel_id),
                room_id    = COALESCE(excluded.room_id,    session_metadata.room_id),
                sender_id  = COALESCE(excluded.sender_id,  session_metadata.sender_id)",
            params![session_key, now, now, channel_id, room_id, sender_id],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn open_conversation(&self, session_key: &str) -> std::io::Result<ChannelConversationRecord> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;
        let now = Utc::now().to_rfc3339();
        tx.execute("INSERT INTO session_metadata(session_key,created_at,last_activity,message_count) VALUES(?1,?2,?3,0) ON CONFLICT(session_key) DO NOTHING",params![session_key,now,now]).map_err(std::io::Error::other)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT conversation_id FROM session_metadata WHERE session_key=?1",
                params![session_key],
                |r| r.get(0),
            )
            .map_err(std::io::Error::other)?;
        let id = if let Some(id) = existing.filter(|s| !s.is_empty()) {
            let u = uuid::Uuid::parse_str(&id)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if u.get_version_num() != 4 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "conversation id is not UUID v4",
                ));
            }
            id
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "UPDATE session_metadata SET conversation_id=?1 WHERE session_key=?2",
                params![id, session_key],
            )
            .map_err(std::io::Error::other)?;
            id
        };
        let history = {
            let mut stmt = tx
                .prepare("SELECT role,content FROM sessions WHERE session_key=?1 ORDER BY id ASC")
                .map_err(std::io::Error::other)?;
            let rows = stmt
                .query_map(params![session_key], |r| {
                    Ok(ChatMessage {
                        role: r.get(0)?,
                        content: r.get(1)?,
                    })
                })
                .map_err(std::io::Error::other)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(std::io::Error::other)?
        };
        tx.commit().map_err(std::io::Error::other)?;
        Ok(ChannelConversationRecord {
            conversation_id: id,
            history,
        })
    }
    fn mutate_conversation_if_current(
        &self,
        session_key: &str,
        expected: &str,
        mutation: SessionMutation<'_>,
    ) -> std::io::Result<ConditionalSessionWrite> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;
        let current: Option<Option<String>> = tx
            .query_row(
                "SELECT conversation_id FROM session_metadata WHERE session_key=?1",
                params![session_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(std::io::Error::other)?;
        let Some(Some(current)) = current else {
            return Ok(ConditionalSessionWrite::Deleted);
        };
        let valid_v4 =
            |id: &str| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.get_version_num() == 4);
        if !valid_v4(&current) || !valid_v4(expected) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "conversation identity is not UUID v4",
            ));
        }
        if current != expected {
            return Ok(ConditionalSessionWrite::Stale);
        }
        let now = Utc::now().to_rfc3339();
        match mutation {
            SessionMutation::Append(m) => {
                tx.execute(
                    "INSERT INTO sessions(session_key,role,content,created_at) VALUES(?1,?2,?3,?4)",
                    params![session_key, m.role, m.content, now],
                )
                .map_err(std::io::Error::other)?;
            }
            SessionMutation::RemoveLast {
                expected_role,
                expected_content,
            } => {
                tx.execute("DELETE FROM sessions WHERE id=(SELECT id FROM sessions WHERE session_key=?1 AND role=?2 AND content=?3 AND id=(SELECT MAX(id) FROM sessions WHERE session_key=?1))",params![session_key,expected_role,expected_content]).map_err(std::io::Error::other)?;
            }
            SessionMutation::UpdateLast(m) => {
                tx.execute("UPDATE sessions SET role=?1,content=?2 WHERE id=(SELECT MAX(id) FROM sessions WHERE session_key=?3)",params![m.role,m.content,session_key]).map_err(std::io::Error::other)?;
            }
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE session_key=?1",
                params![session_key],
                |r| r.get(0),
            )
            .map_err(std::io::Error::other)?;
        tx.execute(
            "UPDATE session_metadata SET message_count=?1,last_activity=?2 WHERE session_key=?3",
            params![count, now, session_key],
        )
        .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(ConditionalSessionWrite::Applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sqlite_schema_probe_and_alter_errors_propagate() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE session_metadata (session_key TEXT)", [])
            .unwrap();
        assert!(ensure_column(&conn, "session_key", "invalid ddl").is_ok());
        let error = ensure_column(
            &conn,
            "missing",
            "ALTER TABLE missing_table ADD COLUMN x TEXT",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to add session_metadata.missing")
        );
    }

    #[test]
    fn fallible_load_distinguishes_missing_history_from_query_failure() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        assert!(backend.load_fallible("missing").unwrap().is_empty());
        backend
            .append("broken", &ChatMessage::user("persisted"))
            .unwrap();
        backend
            .conn
            .lock()
            .execute("DROP TABLE sessions", [])
            .unwrap();

        assert!(backend.load_fallible("broken").is_err());
        assert!(backend.load("broken").is_empty());
    }

    #[test]
    fn round_trip_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .append("user1", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .append("user1", &ChatMessage::assistant("hi"))
            .unwrap();

        let msgs = backend.load("user1");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn remove_last_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("u", &ChatMessage::user("a")).unwrap();
        backend.append("u", &ChatMessage::user("b")).unwrap();

        assert!(backend.remove_last("u").unwrap());
        let msgs = backend.load("u");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "a");
    }

    #[test]
    fn remove_last_empty_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(!backend.remove_last("nonexistent").unwrap());
    }

    #[test]
    fn list_sessions_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("a", &ChatMessage::user("hi")).unwrap();
        backend.append("b", &ChatMessage::user("hey")).unwrap();

        let sessions = backend.list_sessions();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn metadata_tracks_counts() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("a")).unwrap();
        backend.append("s1", &ChatMessage::user("b")).unwrap();
        backend.append("s1", &ChatMessage::user("c")).unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].message_count, 3);
    }

    #[test]
    fn fts5_search_finds_content() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .append(
                "code_chat",
                &ChatMessage::user("How do I parse JSON in Rust?"),
            )
            .unwrap();
        backend
            .append("weather", &ChatMessage::user("What's the weather today?"))
            .unwrap();

        let results = backend.search(&SessionQuery {
            keyword: Some("Rust".into()),
            limit: Some(10),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "code_chat");
    }

    #[test]
    fn fts5_update_trigger_syncs_index() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .append("chat", &ChatMessage::user("hello world"))
            .unwrap();

        // Verify initial content is searchable
        let results = backend.search(&SessionQuery {
            keyword: Some("hello".into()),
            limit: Some(10),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "chat");

        // Directly update the session content (simulates update_last behavior)
        {
            let conn = backend.conn.lock();
            conn.execute(
                "UPDATE sessions SET content = ?1 WHERE session_key = ?2",
                params!["goodbye world", "chat"],
            )
            .unwrap();
        }

        // Old keyword should no longer match
        let results = backend.search(&SessionQuery {
            keyword: Some("hello".into()),
            limit: Some(10),
        });
        assert!(results.is_empty());

        // New keyword should match after UPDATE trigger syncs FTS index
        let results = backend.search(&SessionQuery {
            keyword: Some("goodbye".into()),
            limit: Some(10),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "chat");
    }

    #[test]
    fn cleanup_stale_removes_old_sessions() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        // Insert a session with old timestamp
        {
            let conn = backend.conn.lock();
            let old_time = (Utc::now() - Duration::hours(100)).to_rfc3339();
            conn.execute(
                "INSERT INTO sessions (session_key, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
                params!["old_session", "user", "ancient", old_time],
            ).unwrap();
            conn.execute(
                "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count) VALUES (?1, ?2, ?3, 1)",
                params!["old_session", old_time, old_time],
            ).unwrap();
        }

        backend
            .append("new_session", &ChatMessage::user("fresh"))
            .unwrap();

        let cleaned = backend.cleanup_stale(48).unwrap(); // 48h TTL
        assert_eq!(cleaned, 1);

        let sessions = backend.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], "new_session");
    }

    #[test]
    fn clear_messages_removes_rows_keeps_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s1", &ChatMessage::assistant("hi")).unwrap();
        backend.set_session_name("s1", "My Session").unwrap();

        let cleared = backend.clear_messages("s1").unwrap();
        assert_eq!(cleared, 2);
        assert!(backend.load("s1").is_empty());
        // Session still exists in metadata with name preserved
        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].message_count, 0);
        assert_eq!(meta[0].name.as_deref(), Some("My Session"));
    }

    #[test]
    fn clear_messages_empty_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.clear_messages("nonexistent").unwrap(), 0);
    }

    #[test]
    fn clear_messages_does_not_affect_other_sessions() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s2", &ChatMessage::user("world")).unwrap();

        backend.clear_messages("s1").unwrap();
        assert!(backend.load("s1").is_empty());
        assert_eq!(backend.load("s2").len(), 1);
    }

    #[test]
    fn clear_messages_then_append_works() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("old")).unwrap();
        backend.clear_messages("s1").unwrap();
        backend.append("s1", &ChatMessage::user("new")).unwrap();

        let messages = backend.load("s1");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "new");
        // Metadata count should reflect the new message
        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta[0].message_count, 1);
    }

    #[test]
    fn delete_session_removes_all_data() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s1", &ChatMessage::assistant("hi")).unwrap();
        backend.append("s2", &ChatMessage::user("other")).unwrap();

        assert!(backend.delete_session("s1").unwrap());
        assert!(backend.load("s1").is_empty());
        assert_eq!(backend.list_sessions().len(), 1);
        assert_eq!(backend.list_sessions()[0], "s2");
    }

    #[test]
    fn delete_session_returns_false_for_missing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(!backend.delete_session("nonexistent").unwrap());
    }

    #[test]
    fn session_exists_tracks_metadata_row() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        assert!(!backend.session_exists("ghost"));

        backend
            .append("ghost", &ChatMessage::user("first"))
            .unwrap();
        assert!(backend.session_exists("ghost"));

        assert!(backend.delete_session("ghost").unwrap());
        assert!(!backend.session_exists("ghost"));
    }

    #[test]
    fn migrate_from_jsonl_imports_and_renames() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Create a JSONL file
        let jsonl_path = sessions_dir.join("test_user.jsonl");
        std::fs::write(
            &jsonl_path,
            "{\"role\":\"user\",\"content\":\"hello\"}\n{\"role\":\"assistant\",\"content\":\"hi\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let migrated = backend.migrate_from_jsonl(tmp.path()).unwrap();
        assert_eq!(migrated, 1);

        // JSONL should be renamed
        assert!(!jsonl_path.exists());
        assert!(sessions_dir.join("test_user.jsonl.migrated").exists());

        // Messages should be in SQLite
        let msgs = backend.load("test_user");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn set_session_name_persists() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_name("s1", "My Session").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].name.as_deref(), Some("My Session"));
    }

    #[test]
    fn set_session_name_updates_existing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_name("s1", "First").unwrap();
        backend.set_session_name("s1", "Second").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta[0].name.as_deref(), Some("Second"));
    }

    #[test]
    fn sessions_without_name_return_none() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert!(meta[0].name.is_none());
    }

    // ── session state tests ─────────────────────────────────────────

    #[test]
    fn session_state_idle_to_running() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        let state = backend.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "running");
        assert_eq!(state.turn_id.as_deref(), Some("turn-1"));
        assert!(state.turn_started_at.is_some());
    }

    #[test]
    fn session_state_running_to_idle() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        backend.set_session_state("s1", "idle", None).unwrap();

        let state = backend.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "idle");
        assert!(state.turn_id.is_none());
        assert!(state.turn_started_at.is_none());
    }

    #[test]
    fn session_state_running_to_error() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        backend
            .set_session_state("s1", "error", Some("turn-1"))
            .unwrap();

        let state = backend.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "error");
        assert_eq!(state.turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn list_running_sessions_returns_running_only() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("a")).unwrap();
        backend.append("s2", &ChatMessage::user("b")).unwrap();
        backend.append("s3", &ChatMessage::user("c")).unwrap();

        backend
            .set_session_state("s1", "running", Some("t1"))
            .unwrap();
        backend
            .set_session_state("s2", "running", Some("t2"))
            .unwrap();
        // s3 stays idle (default)

        let running = backend.list_running_sessions();
        assert_eq!(running.len(), 2);
        let keys: Vec<&str> = running.iter().map(|m| m.key.as_str()).collect();
        assert!(keys.contains(&"s1"));
        assert!(keys.contains(&"s2"));
    }

    #[test]
    fn list_stuck_sessions_detects_old_running() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("a")).unwrap();

        // Manually set an old turn_started_at
        {
            let conn = backend.conn.lock();
            let old_time = (Utc::now() - Duration::seconds(600)).to_rfc3339();
            conn.execute(
                "UPDATE session_metadata SET state = 'running', turn_id = 'old', turn_started_at = ?1 WHERE session_key = 's1'",
                params![old_time],
            ).unwrap();
        }

        let stuck = backend.list_stuck_sessions(300); // 5 min threshold
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].key, "s1");

        // Not stuck if threshold is longer
        let not_stuck = backend.list_stuck_sessions(900); // 15 min threshold
        assert_eq!(not_stuck.len(), 0);
    }

    #[test]
    fn get_session_state_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let state = backend.get_session_state("nonexistent").unwrap();
        assert!(state.is_none());
    }

    #[test]
    fn session_state_migration_preserves_data() {
        let tmp = TempDir::new().unwrap();
        // Create backend (runs migration)
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        // Re-open (migration should be idempotent)
        drop(backend);
        let backend2 = SqliteSessionBackend::new(tmp.path()).unwrap();
        let msgs = backend2.load("s1");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");

        // State should default to idle
        let state = backend2.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "idle");
    }

    #[test]
    fn empty_name_clears_to_none() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_name("s1", "Named").unwrap();
        backend.set_session_name("s1", "").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert!(meta[0].name.is_none());
    }

    // ── get_session_metadata tests ─────────────────────────────────

    #[test]
    fn get_session_metadata_returns_full_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s1", &ChatMessage::assistant("hi")).unwrap();
        backend.set_session_name("s1", "My Chat").unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.key, "s1");
        assert_eq!(meta.name.as_deref(), Some("My Chat"));
        assert_eq!(meta.message_count, 2);
    }

    #[test]
    fn get_session_metadata_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(backend.get_session_metadata("nonexistent").is_none());
    }

    #[test]
    fn agent_alias_roundtrips_through_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_agent_alias("s1", "scout").unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.agent_alias.as_deref(), Some("scout"));

        let listed = backend.list_sessions_with_metadata();
        let row = listed.iter().find(|m| m.key == "s1").unwrap();
        assert_eq!(row.agent_alias.as_deref(), Some("scout"));

        // Standalone getter also works.
        let alias = backend.get_session_agent_alias("s1").unwrap();
        assert_eq!(alias.as_deref(), Some("scout"));
    }

    #[test]
    fn rename_agent_attribution_repoints_only_matching_sessions() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hi")).unwrap();
        backend.set_session_agent_alias("s1", "scout").unwrap();
        backend.append("s2", &ChatMessage::user("yo")).unwrap();
        backend.set_session_agent_alias("s2", "other").unwrap();

        // Rename scout → ranger: the conversation history is kept and its
        // attribution follows the renamed agent (contrast clear, which NULLs it).
        let n = backend.rename_agent_attribution("scout", "ranger").unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            backend.get_session_agent_alias("s1").unwrap().as_deref(),
            Some("ranger")
        );
        // unrelated session untouched
        assert_eq!(
            backend.get_session_agent_alias("s2").unwrap().as_deref(),
            Some("other")
        );
        // unknown source → 0
        assert_eq!(backend.rename_agent_attribution("ghost", "x").unwrap(), 0);
    }

    #[test]
    fn agent_alias_set_before_any_append_upserts_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        // No prior append — metadata row does not exist yet. UPSERT
        // path must still record the alias so the WS handshake can
        // attribute the session before the first user message lands.
        backend.set_session_agent_alias("s1", "scout").unwrap();

        let alias = backend.get_session_agent_alias("s1").unwrap();
        assert_eq!(alias.as_deref(), Some("scout"));
    }

    #[test]
    fn session_context_roundtrips_channel_room_sender() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend
            .set_session_context(
                "s1",
                SessionContext {
                    channel_id: Some("discord.clamps"),
                    room_id: Some("1234567890"),
                    sender_id: Some("@user:matrix"),
                },
            )
            .unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.channel_id.as_deref(), Some("discord.clamps"));
        assert_eq!(meta.room_id.as_deref(), Some("1234567890"));
        assert_eq!(meta.sender_id.as_deref(), Some("@user:matrix"));

        // Second call with partial context must NOT clear the columns
        // already filled in — set_session_context is additive.
        backend
            .set_session_context(
                "s1",
                SessionContext {
                    channel_id: None,
                    room_id: Some("1234567890"),
                    sender_id: None,
                },
            )
            .unwrap();
        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.channel_id.as_deref(), Some("discord.clamps"));
        assert_eq!(meta.sender_id.as_deref(), Some("@user:matrix"));
    }

    #[test]
    fn session_context_creates_metadata_row_before_first_append() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .set_session_context(
                "s1",
                SessionContext {
                    channel_id: Some("telegram.production"),
                    room_id: None,
                    sender_id: Some("@alice"),
                },
            )
            .unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.channel_id.as_deref(), Some("telegram.production"));
        assert_eq!(meta.sender_id.as_deref(), Some("@alice"));
        assert!(meta.room_id.is_none());
    }

    #[test]
    fn get_session_metadata_matches_list() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("a")).unwrap();
        backend.append("s1", &ChatMessage::user("b")).unwrap();
        backend.append("s2", &ChatMessage::user("c")).unwrap();

        let single = backend.get_session_metadata("s1").unwrap();
        let all = backend.list_sessions_with_metadata();
        let from_list = all.iter().find(|m| m.key == "s1").unwrap();

        assert_eq!(single.message_count, from_list.message_count);
        assert_eq!(single.name, from_list.name);
        assert_eq!(single.created_at, from_list.created_at);
        assert_eq!(single.last_activity, from_list.last_activity);
    }

    // ── conversation_id (atomic channel identity) tests ───────────────

    #[test]
    fn conversation_id_resolve_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let id1 = backend.resolve_or_create_conversation_id("k").unwrap();
        let id2 = backend.resolve_or_create_conversation_id("k").unwrap();
        assert!(!id1.is_empty());
        assert_eq!(id1, id2, "repeated resolve must return the same id");
    }

    #[test]
    fn conversation_id_legacy_null_row_backfills() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        // Simulate a legacy row created before the conversation_id column
        // existed: a metadata row with conversation_id left NULL and no
        // prior resolve call.
        backend.append("legacy", &ChatMessage::user("old")).unwrap();
        {
            let conn = backend.conn.lock();
            let present: bool = conn
                .query_row(
                    "SELECT conversation_id IS NULL FROM session_metadata \
                     WHERE session_key = 'legacy'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(present, "legacy row must start with NULL conversation_id");
        }

        let id = backend.resolve_or_create_conversation_id("legacy").unwrap();
        assert!(!id.is_empty(), "resolve must backfill a non-empty id");
        // Re-resolve returns the same committed value.
        assert_eq!(
            backend.resolve_or_create_conversation_id("legacy").unwrap(),
            id
        );
    }

    #[test]
    fn conversation_id_survives_reopen() {
        let tmp = TempDir::new().unwrap();
        let id_before = {
            let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
            backend
                .resolve_or_create_conversation_id("persist")
                .unwrap()
        };
        // Reopen the same db file - the id was persisted, not recomputed.
        let backend2 = SqliteSessionBackend::new(tmp.path()).unwrap();
        let id_after = backend2
            .resolve_or_create_conversation_id("persist")
            .unwrap();
        assert_eq!(id_before, id_after);
    }

    #[test]
    fn conversation_id_clear_and_rotate_clears_history_and_mints_new_id() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("rot", &ChatMessage::user("a")).unwrap();
        backend.append("rot", &ChatMessage::assistant("b")).unwrap();
        let id1 = backend.resolve_or_create_conversation_id("rot").unwrap();
        assert_eq!(backend.load("rot").len(), 2);

        let id2 = backend.clear_and_rotate_conversation("rot").unwrap();
        assert_ne!(id1, id2, "rotate must mint a fresh id");
        assert!(backend.load("rot").is_empty(), "rotate must clear history");
        let meta = backend.get_session_metadata("rot").unwrap();
        assert_eq!(meta.message_count, 0);
        assert_eq!(
            meta.conversation_id.as_deref(),
            Some(id2.as_str()),
            "metadata must expose the rotated id"
        );
        // Post-rotate resolve is stable on the new id (rotate is not repeated).
        assert_eq!(
            backend.resolve_or_create_conversation_id("rot").unwrap(),
            id2
        );
    }

    #[test]
    fn conversation_id_other_key_isolation() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let id_a = backend.resolve_or_create_conversation_id("a").unwrap();
        let id_b = backend.resolve_or_create_conversation_id("b").unwrap();
        assert_ne!(id_a, id_b);

        let id_a2 = backend.clear_and_rotate_conversation("a").unwrap();
        assert_ne!(id_a, id_a2);
        // Rotating a must not touch b's id.
        assert_eq!(
            backend.resolve_or_create_conversation_id("b").unwrap(),
            id_b,
            "other-key isolation: rotate(a) must not change b"
        );
    }

    #[test]
    fn conversation_id_delete_then_recreate_mints_new_id() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let id1 = backend.resolve_or_create_conversation_id("del").unwrap();
        assert!(backend.delete_session("del").unwrap());
        let id2 = backend.resolve_or_create_conversation_id("del").unwrap();
        assert_ne!(id1, id2, "delete + recreate must mint a fresh id");
    }

    #[test]
    fn sqlite_open_conversation_concurrent_first_open_converges() {
        use std::sync::{Arc, Barrier};
        let tmp = TempDir::new().unwrap();
        let a = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
        let b = SqliteSessionBackend::new(tmp.path()).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let a2 = Arc::clone(&a);
        let b1 = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            b1.wait();
            a2.open_conversation("same").unwrap().conversation_id
        });
        let b2 = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            b2.wait();
            b.open_conversation("same").unwrap().conversation_id
        });
        assert_eq!(first.join().unwrap(), second.join().unwrap());
    }

    #[test]
    fn conversation_id_resolve_and_rotate_race_stays_consistent() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let tmp = TempDir::new().unwrap();
        let a = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
        let initial = a.resolve_or_create_conversation_id("race").unwrap();
        let b = SqliteSessionBackend::new(tmp.path()).unwrap();
        let barrier = Arc::new(Barrier::new(2));

        // Resolver: spins resolves; every one must observe a valid,
        // linearly-explainable id (either pre-rotate `initial` or the
        // post-rotate id - never empty/corrupt).
        let bar = barrier.clone();
        let a_c = a.clone();
        let h_res = thread::spawn(move || {
            bar.wait();
            let mut ids = Vec::new();
            for _ in 0..64 {
                ids.push(a_c.resolve_or_create_conversation_id("race").unwrap());
            }
            ids
        });

        // Rotator: one atomic clear+rotate.
        let bar2 = barrier.clone();
        let h_rot = thread::spawn(move || {
            bar2.wait();
            b.clear_and_rotate_conversation("race").unwrap()
        });

        let rotated = h_rot.join().unwrap();
        let ids = h_res.join().unwrap();
        assert_ne!(rotated, initial);
        for id in &ids {
            assert!(!id.is_empty(), "race produced an empty id");
            assert!(
                *id == initial || *id == rotated,
                "race produced an id ({id}) that is neither the pre- nor post-rotate value"
            );
        }

        // After both threads joined, the rotate has committed. A fresh
        // instance must observe the post-rotate state: rotated id, empty
        // history.
        let c = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(
            c.resolve_or_create_conversation_id("race").unwrap(),
            rotated,
            "final committed id must be the rotated one"
        );
        assert!(
            c.load("race").is_empty(),
            "rotate must have cleared history"
        );
    }

    // ── crash / delete / migration hardening tests ───────────────────

    #[test]
    fn delete_session_rolls_back_when_metadata_delete_fails() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.conn.lock().execute(
            "CREATE TRIGGER fail_metadata_delete BEFORE DELETE ON session_metadata BEGIN SELECT RAISE(ABORT, 'injected failure'); END", [],
        ).unwrap();
        assert!(backend.delete_session("s1").is_err());
        assert_eq!(backend.load("s1").len(), 1);
        assert!(backend.session_exists("s1"));
    }

    #[test]
    fn migrate_from_single_jsonl_snapshot_preserves_id_and_history() {
        let tmp = TempDir::new().unwrap();
        let source = crate::session_store::SessionStore::new(tmp.path()).unwrap();
        let opened = source.open_conversation("snapshot").unwrap();
        source
            .append("snapshot", &ChatMessage::user("hello"))
            .unwrap();
        source
            .append("snapshot", &ChatMessage::assistant("hi"))
            .unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 1);
        let migrated = backend.open_conversation("snapshot").unwrap();
        assert_eq!(migrated.conversation_id, opened.conversation_id);
        assert_eq!(migrated.history.len(), 2);
        assert!(tmp.path().join("sessions/snapshot.jsonl.migrated").exists());
    }

    #[test]
    fn migrate_existing_target_skips_without_upgrading_or_renaming_source() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let source_path = sessions.join("existing.jsonl");
        std::fs::write(&source_path, "{\"role\":\"user\",\"content\":\"legacy\"}\n").unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.open_conversation("existing").unwrap();
        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 0);
        assert!(source_path.exists());
        assert!(!source_path.with_extension("jsonl.migrated").exists());
        assert!(
            !std::fs::read_to_string(source_path)
                .unwrap()
                .contains("session_meta")
        );
    }

    // ── conditional-write (conversation-id fence) tests ───────────────

    #[test]
    fn channel_conversation_contract_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &backend;
        crate::session_backend::assert_channel_conversation_contract(backend);
    }

    #[test]
    fn sqlite_legacy_rollback_updates_message_count_atomically() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let key = "legacy_rollback";
        backend.append(key, &ChatMessage::user("one")).unwrap();
        backend.append(key, &ChatMessage::assistant("two")).unwrap();
        let id = backend.open_conversation(key).unwrap().conversation_id;
        assert_eq!(
            backend
                .remove_last_if_conversation_matches(key, &id)
                .unwrap(),
            ConditionalSessionWrite::Applied
        );
        assert_eq!(backend.load(key).len(), 1);
        assert_eq!(backend.get_session_metadata(key).unwrap().message_count, 1);
        assert_eq!(
            backend
                .remove_last_if_conversation_matches(key, &id)
                .unwrap(),
            ConditionalSessionWrite::Applied
        );
        assert!(backend.load(key).is_empty());
        assert_eq!(backend.get_session_metadata(key).unwrap().message_count, 0);
        assert_eq!(
            backend
                .remove_last_if_conversation_matches(key, &id)
                .unwrap(),
            ConditionalSessionWrite::Applied
        );
        assert_eq!(
            backend
                .remove_last_if_conversation_matches(key, &uuid::Uuid::new_v4().to_string())
                .unwrap(),
            ConditionalSessionWrite::Stale
        );
        backend.delete_session(key).unwrap();
        assert_eq!(
            backend
                .remove_last_if_conversation_matches(key, &id)
                .unwrap(),
            ConditionalSessionWrite::Deleted
        );
    }

    #[test]
    fn conditional_write_rejects_malformed_conversation_ids() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let key = "malformed";
        let record = backend.open_conversation(key).unwrap();
        assert!(
            backend
                .mutate_conversation_if_current(
                    key,
                    "not-a-uuid",
                    SessionMutation::Append(&ChatMessage::user("bad")),
                )
                .is_err()
        );
        backend
            .conn
            .lock()
            .execute(
                "UPDATE session_metadata SET conversation_id = 'also-bad' WHERE session_key = ?1",
                params![key],
            )
            .unwrap();
        assert!(
            backend
                .mutate_conversation_if_current(
                    key,
                    &record.conversation_id,
                    SessionMutation::Append(&ChatMessage::user("bad")),
                )
                .is_err()
        );
        assert!(backend.load(key).is_empty());
    }

    #[test]
    fn conditional_write_append_propagates_real_db_error() {
        // Inject a REAL failure: a BEFORE INSERT trigger on `sessions` raises
        // ABORT, so the message INSERT inside `append_if_conversation_matches`
        // fails AFTER the classify read returned Applied. The error MUST
        // propagate as `Err` - it must NOT degrade to `Stale` or `Deleted`
        // (which would silently swallow a storage fault as a lifecycle race).
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let key = "fail_append";
        let id = backend.resolve_or_create_conversation_id(key).unwrap();

        {
            let conn = backend.conn.lock();
            conn.execute(
                "CREATE TRIGGER fail_session_insert BEFORE INSERT ON sessions
                 BEGIN
                     SELECT RAISE(ABORT, 'injected failure');
                 END",
                [],
            )
            .unwrap();
        }

        let result = backend.append_if_conversation_matches(key, &id, &ChatMessage::user("boom"));
        assert!(
            result.is_err(),
            "a real DB error must propagate, not degrade to a lifecycle status"
        );
    }

    #[test]
    fn conditional_write_update_last_is_noop_on_empty_matching_record() {
        // A matching record with no messages reports `Applied` (no-op mutation);
        // the caller's preconditions remain the source of truth.
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let key = "empty_match";
        let id = backend.resolve_or_create_conversation_id(key).unwrap();
        assert_eq!(
            backend
                .update_last_if_conversation_matches(key, &id, &ChatMessage::assistant("x"))
                .unwrap(),
            ConditionalSessionWrite::Applied
        );
        assert!(backend.load(key).is_empty());
    }
}
