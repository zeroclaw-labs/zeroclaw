//! Legacy SQLite test fixture. Production migration uses `upgrade::readers`.
//!
//! The old on-disk `name` column is translated to the canonical `title` field
//! here. Normal session reads and writes use the event catalog instead.

use crate::session_events::upgrade::fixtures::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use zeroclaw_api::model_provider::ChatMessage;

/// How long opening the session store waits on another connection that holds
/// the database.
const OPEN_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// SQLite-backed session store with FTS5 and WAL mode.
pub struct SqliteSessionBackend {
    sessions_dir: PathBuf,
    conn: Mutex<Connection>,
}

impl SqliteSessionBackend {
    /// Where the legacy store for `workspace_dir` lives, whether or not it
    /// exists. Startup recovery uses this path without opening the backend.
    pub fn db_path(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join("sessions").join("sessions.db")
    }

    /// Settle turns left marked `running` by a previous process. The caller
    /// must already own the data directory and invoke this before admitting a
    /// new turn. The transcript remains intact and no turn is restarted.
    pub fn recover_abandoned_turns_at(workspace_dir: &Path) -> std::io::Result<usize> {
        crate::session_events::upgrade::readers::recover_abandoned_turns_at(workspace_dir)
    }
}

impl SqliteSessionBackend {
    /// Open or create the sessions database.
    ///
    /// A daemon's gateway, channels and RPC plane each open this database
    /// when they start, so on a fresh data dir their first opens race. The
    /// switch to WAL is retried while another opener holds the file, and the
    /// schema and column migration run in one IMMEDIATE transaction: a later
    /// opener waits for the first and then finds every column in place.
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let guard =
            crate::session_events::installation::legacy_guard(&workspace_dir.join("sessions"))?;
        Self::open_locked(&guard)
    }

    /// The borrowed installation fence covers schema initialization and source
    /// reads during upgrade. Public constructors admit legacy mode first.
    pub(crate) fn open_locked(
        guard: &crate::session_events::installation::InstallationGuard,
    ) -> Result<Self> {
        let sessions_dir = guard.directory().to_owned();
        let db_path = sessions_dir.join("sessions.db");

        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open session DB: {}", db_path.display()))?;
        conn.busy_timeout(OPEN_BUSY_TIMEOUT)
            .context("Failed to set the session DB busy timeout")?;

        Self::enable_wal(&conn)?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 4194304;",
        )?;

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to lock the session DB for schema setup")?;
        tx.execute_batch(
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
                "trim_breadcrumb",
                "ALTER TABLE session_metadata ADD COLUMN trim_breadcrumb INTEGER",
            ),
            (
                "principal_id",
                "ALTER TABLE session_metadata ADD COLUMN principal_id TEXT",
            ),
        ] {
            Self::ensure_metadata_column(&tx, column, ddl)?;
        }
        for (index, ddl) in [
            (
                "idx_session_metadata_agent_alias",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_agent_alias \
                 ON session_metadata(agent_alias)",
            ),
            (
                "idx_session_metadata_channel_id",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_channel_id \
                 ON session_metadata(channel_id)",
            ),
            (
                "idx_session_metadata_room_id",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_room_id \
                 ON session_metadata(room_id)",
            ),
            (
                "idx_session_metadata_sender_id",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_sender_id \
                 ON session_metadata(sender_id)",
            ),
            (
                "idx_session_metadata_principal_id",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_principal_id \
                 ON session_metadata(principal_id)",
            ),
        ] {
            tx.execute(ddl, [])
                .with_context(|| format!("Failed to create session index {index}"))?;
        }
        tx.commit().context("Failed to commit the session schema")?;

        Ok(Self {
            sessions_dir,
            conn: Mutex::new(conn),
        })
    }

    /// Switch the database to WAL, retrying while another connection holds it.
    ///
    /// A second first-time opener of a fresh file can get `SQLITE_BUSY` from
    /// the journal-mode change at once: SQLite skips the busy handler where
    /// waiting could deadlock, so the busy timeout alone does not cover it.
    fn enable_wal(conn: &Connection) -> Result<()> {
        let deadline = std::time::Instant::now() + OPEN_BUSY_TIMEOUT;
        let mut pause = std::time::Duration::from_millis(5);
        loop {
            match conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(_) => return Ok(()),
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::DatabaseBusy
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(std::time::Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(error).context("Failed to switch the session DB to WAL");
                }
            }
        }
    }

    fn ensure_metadata_column(conn: &Connection, column: &str, ddl: &str) -> Result<()> {
        let present: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') \
                 WHERE name = ?1",
                params![column],
                |row| row.get(0),
            )
            .with_context(|| format!("Failed to inspect session metadata column {column}"))?;
        if !present {
            conn.execute(ddl, [])
                .with_context(|| format!("Failed to add session metadata column {column}"))?;
        }
        Ok(())
    }

    fn append_on(
        conn: &Connection,
        session_key: &str,
        message: &ChatMessage,
        now: &str,
    ) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO sessions (session_key, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_key, message.role, message.content, now],
        )?;
        conn.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(session_key) DO UPDATE SET
                last_activity = excluded.last_activity,
                message_count = message_count + 1",
            params![session_key, now, now],
        )?;
        Ok(())
    }

    fn rewrite_messages_on(
        conn: &Connection,
        session_key: &str,
        messages: &[ChatMessage],
    ) -> rusqlite::Result<()> {
        conn.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )?;
        let now = Utc::now().to_rfc3339();
        for message in messages {
            Self::append_on(conn, session_key, message, &now)?;
        }
        conn.execute(
            "UPDATE session_metadata SET message_count = ?2 WHERE session_key = ?1",
            params![session_key, messages.len() as i64],
        )?;
        Ok(())
    }

    fn set_session_trim_breadcrumb_on(
        conn: &Connection,
        session_key: &str,
        present: bool,
    ) -> rusqlite::Result<()> {
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count, trim_breadcrumb)
             VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT(session_key) DO UPDATE SET trim_breadcrumb = excluded.trim_breadcrumb",
            params![session_key, now, now, i64::from(present)],
        )?;
        Ok(())
    }
}

impl SessionBackend for SqliteSessionBackend {
    fn require_legacy_access(&self) -> std::io::Result<()> {
        crate::session_events::installation::legacy_guard(&self.sessions_dir).map(|_| ())
    }

    fn try_load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare("SELECT role, content FROM sessions WHERE session_key = ?1 ORDER BY id ASC")
            .map_err(std::io::Error::other)?;
        let rows = statement
            .query_map(params![session_key], |row| {
                Ok(ChatMessage {
                    tool_calls: Vec::new(),
                    tool_result: None,
                    reasoning_content: None,
                    role: row.get(0)?,
                    content: row.get(1)?,

                    attachments: Vec::new(),
                })
            })
            .map_err(std::io::Error::other)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(std::io::Error::other)
    }
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let conn = self.conn.lock();
        let mut stmt = match conn
            .prepare("SELECT role, content FROM sessions WHERE session_key = ?1 ORDER BY id ASC")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map(params![session_key], |row| {
            Ok(ChatMessage {
                tool_calls: Vec::new(),
                tool_result: None,
                reasoning_content: None,
                role: row.get(0)?,
                content: row.get(1)?,

                attachments: Vec::new(),
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn load_with_timestamps(
        &self,
        session_key: &str,
    ) -> Vec<crate::session_events::upgrade::fixtures::session_backend::TimestampedMessage> {
        use crate::session_events::upgrade::fixtures::session_backend::TimestampedMessage;
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
                message: ChatMessage {
                    tool_calls: Vec::new(),
                    tool_result: None,
                    reasoning_content: None,
                    role,
                    content,
                    attachments: Vec::new(),
                },
                created_at,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        Self::append_on(&conn, session_key, message, &now).map_err(std::io::Error::other)
    }

    fn rewrite_messages(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(std::io::Error::other)?;
        Self::rewrite_messages_on(&tx, session_key, messages).map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(())
    }

    /// Replace the transcript and the breadcrumb flag in one transaction, so
    /// the pair can never commit as a partial write: a crash or error
    /// between the two rolls the whole transaction back rather than leaving
    /// a new transcript paired with a stale flag (or vice versa). The
    /// default `SessionBackend::replace_conversation_state` performs these
    /// as two separate committed operations; this override replaces it for
    /// SQLite where a single transaction can make the pair atomic.
    fn replace_conversation_state(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(std::io::Error::other)?;
        Self::rewrite_messages_on(&tx, session_key, messages).map_err(std::io::Error::other)?;
        Self::set_session_trim_breadcrumb_on(&tx, session_key, breadcrumb_present)
            .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(())
    }

    /// Existence-guarded replacement in one transaction: when the metadata
    /// row is gone (a concurrent delete won), return `Ok(false)` without
    /// writing anything, so a completed turn can never recreate a session
    /// the user just deleted. The plain `replace_conversation_state`
    /// breadcrumb upsert would `INSERT` a fresh metadata row here.
    /// `delete_session` holds the same connection mutex, so the existence
    /// probe and both writes are atomic with respect to it. A genuine query
    /// failure (anything other than "no such row") is still an error, not a
    /// silent skip: only a missing row means "deleted".
    fn replace_conversation_state_if_exists(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<bool> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(std::io::Error::other)?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM session_metadata WHERE session_key = ?1 LIMIT 1",
                params![session_key],
                |_| Ok(()),
            )
            .optional()
            .map_err(std::io::Error::other)?
            .is_some();
        if !exists {
            return Ok(false);
        }
        Self::rewrite_messages_on(&tx, session_key, messages).map_err(std::io::Error::other)?;
        Self::set_session_trim_breadcrumb_on(&tx, session_key, breadcrumb_present)
            .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(true)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, principal_id
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
            let principal_id: Option<String> = row.get(9)?;

            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                title: name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
                principal_id,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.conn.lock();

        conn.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        let count = conn.changes() as usize;

        // Reset trim_breadcrumb to NULL (unrecorded) regardless of whether
        // any rows were deleted: a reset transcript has no synthetic marker,
        // and a stale recorded `true` on an already-empty session must not
        // survive to misclassify the next first message.
        conn.execute(
            "UPDATE session_metadata SET message_count = 0, last_activity = ?1, trim_breadcrumb = NULL WHERE session_key = ?2",
            params![Utc::now().to_rfc3339(), session_key],
        )
        .map_err(std::io::Error::other)?;

        Ok(count)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.conn.lock();

        // Check if session exists
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM session_metadata WHERE session_key = ?1",
                params![session_key],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !exists {
            return Ok(false);
        }

        // Delete messages (FTS5 trigger handles sessions_fts cleanup)
        conn.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        // Delete metadata
        conn.execute(
            "DELETE FROM session_metadata WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    fn delete_session_owned(
        &self,
        session_key: &str,
        owner_principal_id: &str,
    ) -> std::io::Result<bool> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.conn.lock();
        // The predicated metadata delete IS the ownership check: zero rows
        // means not-owned (or missing) and nothing else is touched.
        let owned_rows = conn
            .execute(
                "DELETE FROM session_metadata WHERE session_key = ?1 AND principal_id = ?2",
                params![session_key, owner_principal_id],
            )
            .map_err(std::io::Error::other)?;
        if owned_rows == 0 {
            return Ok(false);
        }
        // Ownership proven and consumed; clean up the message rows (the
        // FTS5 trigger handles sessions_fts).
        conn.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(true)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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

    fn set_session_title(&self, session_key: &str, title: &str) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.conn.lock();
        let title = if title.is_empty() { None } else { Some(title) };
        conn.execute(
            "UPDATE session_metadata SET name = ?1 WHERE session_key = ?2",
            params![title, session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_title(&self, session_key: &str) -> std::io::Result<Option<String>> {
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, principal_id
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
                let principal_id: Option<String> = row.get(9)?;

                let created = DateTime::parse_from_rfc3339(&created_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let activity = DateTime::parse_from_rfc3339(&activity_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                Ok(SessionMetadata {
                    key,
                    title: name,
                    created_at: created,
                    last_activity: activity,
                    message_count: count as usize,
                    agent_alias,
                    channel_id,
                    room_id,
                    sender_id,
                    principal_id,
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, principal_id
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
            let principal_id: Option<String> = row.get(9)?;
            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                title: name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
                principal_id,
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, principal_id
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
            let principal_id: Option<String> = row.get(9)?;
            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                title: name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
                principal_id,
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
                    "SELECT created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id, principal_id FROM session_metadata WHERE session_key = ?1",
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
                        let principal_id: Option<String> = row.get(8)?;
                        Ok(SessionMetadata {
                            key: key.clone(),
                            title: name,
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
                            principal_id,
                        })
                    },
                )
                .ok()
            })
            .collect()
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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

    fn set_session_principal(&self, session_key: &str, principal_id: &str) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.conn.lock();
        let principal_val = if principal_id.is_empty() {
            None
        } else {
            Some(principal_id)
        };
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count, principal_id)
             VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT(session_key) DO UPDATE SET principal_id = excluded.principal_id",
            params![session_key, now, now, principal_val],
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

    fn set_session_trim_breadcrumb(&self, session_key: &str, present: bool) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.conn.lock();
        Self::set_session_trim_breadcrumb_on(&conn, session_key, present)
            .map_err(std::io::Error::other)
    }

    fn get_session_trim_breadcrumb(&self, session_key: &str) -> std::io::Result<Option<bool>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT trim_breadcrumb FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| row.get::<_, Option<i64>>(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(std::io::Error::other(other)),
        })
        .map(|v| v.map(|n| n != 0))
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

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
        backend.set_session_title("s1", "My Session").unwrap();

        let cleared = backend.clear_messages("s1").unwrap();
        assert_eq!(cleared, 2);
        assert!(backend.load("s1").is_empty());
        // Session still exists in metadata with name preserved
        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].message_count, 0);
        assert_eq!(meta[0].title.as_deref(), Some("My Session"));
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
    fn clear_messages_resets_trim_breadcrumb() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_trim_breadcrumb("s1", true).unwrap();
        assert_eq!(
            backend.get_session_trim_breadcrumb("s1").unwrap(),
            Some(true)
        );

        backend.clear_messages("s1").unwrap();

        // A reset session has no synthetic marker; the recorded flag must
        // not survive as a stale `true` for the next first message.
        assert_eq!(backend.get_session_trim_breadcrumb("s1").unwrap(), None);
    }

    #[test]
    fn clear_messages_resets_trim_breadcrumb_even_when_already_empty() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_trim_breadcrumb("s1", true).unwrap();
        backend.clear_messages("s1").unwrap();
        assert_eq!(backend.get_session_trim_breadcrumb("s1").unwrap(), None);

        // Clearing an already-empty session must not leave a stale flag
        // behind either.
        backend.set_session_trim_breadcrumb("s1", true).unwrap();
        assert_eq!(backend.clear_messages("s1").unwrap(), 0);
        assert_eq!(backend.get_session_trim_breadcrumb("s1").unwrap(), None);
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
    fn owned_delete_is_an_atomic_ownership_predicate() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_principal("s1", "user:alice").unwrap();

        // The wrong owner deletes nothing and leaves the session intact.
        assert!(!backend.delete_session_owned("s1", "user:bob").unwrap());
        assert!(!backend.load("s1").is_empty());
        assert_eq!(
            backend
                .get_session_metadata("s1")
                .and_then(|m| m.principal_id),
            Some("user:alice".to_string())
        );

        // The owner's predicated delete removes the session and its rows.
        assert!(backend.delete_session_owned("s1", "user:alice").unwrap());
        assert!(backend.load("s1").is_empty());
        assert!(backend.get_session_metadata("s1").is_none());
    }

    #[test]
    fn owned_delete_refuses_null_owner_rows() {
        // A legacy row with no owner is not deletable through the owned
        // path: NULL never equals a principal id, so scoped principals
        // cannot destroy pre-principal sessions.
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("legacy", &ChatMessage::user("old")).unwrap();
        assert!(
            !backend
                .delete_session_owned("legacy", "user:alice")
                .unwrap()
        );
        assert!(!backend.load("legacy").is_empty());
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
    fn new_rejects_schema_index_name_collisions() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let conn = Connection::open(sessions_dir.join("sessions.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE idx_session_metadata_agent_alias (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        drop(conn);

        let err = SqliteSessionBackend::new(tmp.path())
            .err()
            .expect("index name collision must fail startup");
        assert!(err.to_string().contains("idx_session_metadata_agent_alias"));
    }

    /// A daemon's gateway, channels and RPC plane open the store together
    /// when it starts. On a fresh data dir every one of them must get a
    /// working store, not a lost `ALTER` race or a busy journal-mode switch.
    #[test]
    fn concurrent_first_openers_of_a_fresh_store_all_succeed() {
        const TRIALS: usize = 40;
        const OPENERS: usize = 4;
        for trial in 0..TRIALS {
            let tmp = TempDir::new().unwrap();
            let start = Arc::new(std::sync::Barrier::new(OPENERS));
            let openers: Vec<_> = (0..OPENERS)
                .map(|opener| {
                    let (dir, start) = (tmp.path().to_path_buf(), Arc::clone(&start));
                    std::thread::spawn(move || {
                        start.wait();
                        let backend = SqliteSessionBackend::new(&dir).map_err(|error| {
                            format!("trial {trial}, opener {opener}: {error:#}")
                        })?;
                        backend
                            .append(&format!("opener-{opener}"), &ChatMessage::user("hello"))
                            .map_err(|error| {
                                format!("trial {trial}, opener {opener} append: {error:#}")
                            })
                    })
                })
                .collect();
            for opener in openers {
                opener.join().unwrap().unwrap();
            }
            let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
            assert_eq!(backend.list_sessions().len(), OPENERS);
        }
    }

    #[test]
    fn set_session_title_persists() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_title("s1", "My Session").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].title.as_deref(), Some("My Session"));
    }

    #[test]
    fn set_session_title_updates_existing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_title("s1", "First").unwrap();
        backend.set_session_title("s1", "Second").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta[0].title.as_deref(), Some("Second"));
    }

    #[test]
    fn sessions_without_name_return_none() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert!(meta[0].title.is_none());
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
    fn schema_migration_repairs_individually_missing_state_columns() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let db_path = sessions_dir.join("sessions.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_metadata (
                session_key TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                last_activity TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0,
                name TEXT,
                state TEXT NOT NULL DEFAULT 'idle'
            );",
        )
        .unwrap();
        drop(conn);

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        drop(backend);
        let conn = Connection::open(db_path).unwrap();
        for column in ["turn_id", "turn_started_at"] {
            let present: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') \
                     WHERE name = ?1",
                    params![column],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(present, "missing repaired column {column}");
        }
    }

    #[test]
    fn empty_name_clears_to_none() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_title("s1", "Named").unwrap();
        backend.set_session_title("s1", "").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert!(meta[0].title.is_none());
    }

    // ── get_session_metadata tests ─────────────────────────────────

    #[test]
    fn get_session_metadata_returns_full_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s1", &ChatMessage::assistant("hi")).unwrap();
        backend.set_session_title("s1", "My Chat").unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.key, "s1");
        assert_eq!(meta.title.as_deref(), Some("My Chat"));
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
        assert_eq!(single.title, from_list.title);
        assert_eq!(single.created_at, from_list.created_at);
        assert_eq!(single.last_activity, from_list.last_activity);
    }

    #[test]
    fn replace_conversation_state_if_exists_is_a_no_op_once_deletion_wins() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("gone", &ChatMessage::user("turn")).unwrap();
        assert!(backend.delete_session("gone").unwrap());

        // Deterministic delete-versus-completion ordering: the deleter
        // commits first, then the post-turn write arrives. It must become a
        // no-op and must not recreate the metadata row the delete removed
        // (the plain replace's breadcrumb upsert would INSERT one).
        let written = backend
            .replace_conversation_state_if_exists("gone", &[ChatMessage::user("late turn")], false)
            .unwrap();
        assert!(
            !written,
            "post-turn write must be a no-op once deletion wins"
        );
        assert!(!backend.session_exists("gone"));
        assert!(backend.load("gone").is_empty());

        // The live path still writes transcript and flag together.
        backend.append("live", &ChatMessage::user("turn")).unwrap();
        let written = backend
            .replace_conversation_state_if_exists("live", &[ChatMessage::user("new")], true)
            .unwrap();
        assert!(written);
        assert_eq!(backend.load("live").len(), 1);
        assert_eq!(
            backend.get_session_trim_breadcrumb("live").unwrap(),
            Some(true)
        );
    }

    #[test]
    fn replace_conversation_state_is_atomic() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .append("s1", &ChatMessage::user("pre-replace turn"))
            .unwrap();
        backend.set_session_trim_breadcrumb("s1", false).unwrap();

        // A trigger that fails the breadcrumb half of the write, so the call
        // has to roll back the transcript half too if it is truly one
        // transaction rather than two committed statements.
        {
            let conn = backend.conn.lock();
            conn.execute_batch(
                "CREATE TRIGGER poison_trim_breadcrumb
                 BEFORE UPDATE OF trim_breadcrumb ON session_metadata
                 WHEN NEW.session_key = 's1'
                 BEGIN
                     SELECT RAISE(ABORT, 'simulated breadcrumb write failure');
                 END;",
            )
            .unwrap();
        }

        let result = backend.replace_conversation_state(
            "s1",
            &[ChatMessage::user(
                "replacement turn that must not land alone",
            )],
            true,
        );
        assert!(result.is_err(), "the poisoned trigger must fail the call");

        // The transcript half must have rolled back with the breadcrumb
        // half, not landed as a new transcript paired with the old flag.
        let messages = backend.load("s1");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "pre-replace turn");
        assert_eq!(
            backend.get_session_trim_breadcrumb("s1").unwrap(),
            Some(false)
        );
    }
}
