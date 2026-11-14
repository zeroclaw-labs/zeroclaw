//! SQLite-backed session persistence with FTS5 search.

use crate::session_backend::{
    REQUEST_RECEIPT_PROTECTION_MINUTES, RequestReceipt, SessionBackend, SessionContext,
    SessionMetadata, SessionQuery, SessionState,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use zeroclaw_api::model_provider::ChatMessage;

pub mod agent_tasks;
mod bridge_intake;
mod delegation;
pub mod reminder_occurrences;
#[cfg(test)]
mod review_source_tests;

/// Request receipts kept per session (see `record_request`).
const REQUEST_RECEIPTS_PER_SESSION: i64 = 256;
/// Hard bound on the whole `session_requests` table. Session keys are
/// client-supplied, so the per-session cap alone cannot bound the table when
/// `session_ttl_hours` is 0 (the default: no sweep ever runs). Under abuse the
/// receipts older than the protected retry window may be reclaimed. While
/// all receipts are protected, new inputs fail closed; no session data is touched.
const REQUEST_RECEIPTS_GLOBAL_CAP: i64 = 16384;

/// How long opening the session store waits on another connection that holds
/// the database.
const OPEN_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// SQLite-backed session store with FTS5 and WAL mode.
pub struct SqliteSessionBackend {
    conn: Mutex<Connection>,
}

impl SqliteSessionBackend {
    /// Called only by the exclusive gateway owner, before it admits turns.
    /// Opening a reader/backend must never settle another owner's live work.
    pub(crate) fn settle_abandoned_gateway_turns(data_dir: &Path) -> std::io::Result<usize> {
        let path = Self::database_path(data_dir);
        if !path.try_exists()? {
            return Ok(0);
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(std::io::Error::other)?;
        conn.busy_timeout(OPEN_BUSY_TIMEOUT)
            .map_err(std::io::Error::other)?;
        let has_state: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') WHERE name = 'state'",
            [], |row| row.get(0),
        ).map_err(std::io::Error::other)?;
        if !has_state {
            return Ok(0);
        }
        // No migration, replay, history rewrite or non-gateway state change.
        conn.execute(
            "UPDATE session_metadata SET state = 'error', turn_started_at = NULL
             WHERE substr(session_key, 1, 3) = 'gw_' AND state = 'running'",
            [],
        )
        .map_err(std::io::Error::other)
    }

    pub(crate) fn database_path(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join("sessions").join("sessions.db")
    }

    /// Open or create the sessions database.
    ///
    /// The daemon's gateway, channels and bridge intake each open this
    /// database when they start, so on a fresh data dir their first opens
    /// race. The switch to WAL is retried while another opener holds the
    /// file, and the schema and column migrations run in one IMMEDIATE
    /// transaction: a later opener waits for the first and then finds every
    /// column in place.
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).context("Failed to create sessions directory")?;
        let db_path = Self::database_path(workspace_dir);

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
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
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

             CREATE TABLE IF NOT EXISTS session_requests (
                session_key TEXT NOT NULL,
                request_id  TEXT NOT NULL,
                state       TEXT NOT NULL,
                accepted_at TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                PRIMARY KEY (session_key, request_id)
             );

             CREATE TABLE IF NOT EXISTS session_delegations (
                agent_alias    TEXT NOT NULL,
                request_id     TEXT NOT NULL,
                request_digest TEXT NOT NULL,
                dispatch_id    TEXT,
                route_digest   TEXT,
                PRIMARY KEY (agent_alias, request_id)
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

        bridge_intake::initialize(&tx)?;

        // Historical messages have no attributable ingress source. Leave
        // them NULL rather than guessing from the session's latest sender.
        let has_source: bool = tx.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('sessions') WHERE name = 'message_source'",
            [],
            |row| row.get(0),
        )?;
        if !has_source {
            tx.execute("ALTER TABLE sessions ADD COLUMN message_source TEXT", [])
                .context("Failed to add session message source")?;
        }

        // Migration: add name column to existing databases
        let has_name: bool = tx
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') WHERE name = 'name'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_name {
            let _ = tx.execute("ALTER TABLE session_metadata ADD COLUMN name TEXT", []);
        }

        // Migration: add state tracking columns
        let has_state: bool = tx
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') WHERE name = 'state'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_state {
            let _ = tx.execute(
                "ALTER TABLE session_metadata ADD COLUMN state TEXT NOT NULL DEFAULT 'idle'",
                [],
            );
            let _ = tx.execute("ALTER TABLE session_metadata ADD COLUMN turn_id TEXT", []);
            let _ = tx.execute(
                "ALTER TABLE session_metadata ADD COLUMN turn_started_at TEXT",
                [],
            );
        }

        // Migration: add agent_alias column for per-agent attribution
        let has_agent_alias: bool = tx
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') WHERE name = 'agent_alias'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_agent_alias {
            let _ = tx.execute(
                "ALTER TABLE session_metadata ADD COLUMN agent_alias TEXT",
                [],
            );
            let _ = tx.execute(
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_agent_alias \
                 ON session_metadata(agent_alias)",
                [],
            );
        }

        for (column, ddl) in [
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
        ] {
            let present: bool = tx
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') \
                     WHERE name = ?1",
                    params![column],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if !present {
                let _ = tx.execute(ddl, []);
            }
        }
        let _ = tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_channel_id \
             ON session_metadata(channel_id)",
            [],
        );
        let _ = tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_room_id \
             ON session_metadata(room_id)",
            [],
        );
        let _ = tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_sender_id \
             ON session_metadata(sender_id)",
            [],
        );

        tx.commit().context("Failed to commit the session schema")?;
        // Opens its own IMMEDIATE transaction, so it runs after the schema
        // transaction rather than inside it; idempotent for late openers.
        delegation::migrate_route_provenance(&mut conn)
            .context("Failed to migrate session schema")?;
        agent_tasks::migrate_agent_tasks(&mut conn)
            .context("Failed to migrate agent task schema")?;
        reminder_occurrences::migrate_reminder_occurrences(&mut conn)
            .context("Failed to migrate reminder occurrence schema")?;

        crate::sqlite_perms::harden_sqlite_owner_only(&db_path);
        Ok(Self {
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

    fn append_message(
        &self,
        session_key: &str,
        message: &ChatMessage,
        ingress: Option<&zeroclaw_api::review::UserMessageIngress>,
    ) -> std::io::Result<()> {
        if ingress.is_some() && message.role != "user" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "only user messages carry an ingress source",
            ));
        }
        let source = ingress
            .map(serde_json::to_string)
            .transpose()
            .map_err(std::io::Error::other)?;
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO sessions (session_key, role, content, created_at, message_source)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_key, message.role, message.content, now, source],
        )
        .map_err(std::io::Error::other)?;
        tx.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(session_key) DO UPDATE SET
                last_activity = excluded.last_activity,
                message_count = message_count + 1",
            params![session_key, now, now],
        )
        .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)
    }

    /// Migrate JSONL session files into SQLite. Renames migrated files to `.jsonl.migrated`.
    pub fn migrate_from_jsonl(&self, workspace_dir: &Path) -> Result<usize> {
        let sessions_dir = workspace_dir.join("sessions");
        let entries = match std::fs::read_dir(&sessions_dir) {
            Ok(e) => e,
            Err(_) => return Ok(0),
        };

        let mut migrated = 0;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            let Some(key) = name.strip_suffix(".jsonl") else {
                continue;
            };

            let path = entry.path();
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let reader = std::io::BufReader::new(file);
            let mut count = 0;
            for line in std::io::BufRead::lines(reader) {
                let Ok(line) = line else { continue };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(msg) = serde_json::from_str::<ChatMessage>(trimmed)
                    && self.append(key, &msg).is_ok()
                {
                    count += 1;
                }
            }

            if count > 0 {
                let migrated_path = path.with_extension("jsonl.migrated");
                let _ = std::fs::rename(&path, &migrated_path);
                migrated += 1;
            }
        }

        Ok(migrated)
    }
}

impl SessionBackend for SqliteSessionBackend {
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
                role: row.get(0)?,
                content: row.get(1)?,
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
    ) -> Vec<crate::session_backend::TimestampedMessage> {
        use crate::session_backend::TimestampedMessage;
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT role, content, created_at, message_source FROM sessions WHERE session_key = ?1 ORDER BY id ASC",
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
                ingress: row
                    .get::<_, Option<String>>(3)?
                    .and_then(|raw| serde_json::from_str(&raw).ok()),
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.append_message(session_key, message, None)
    }

    fn append_with_ingress(
        &self,
        session_key: &str,
        message: &ChatMessage,
        ingress: &zeroclaw_api::review::UserMessageIngress,
    ) -> std::io::Result<()> {
        self.append_message(session_key, message, Some(ingress))
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
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
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;
        let cutoff = (Utc::now() - Duration::hours(i64::from(ttl_hours))).to_rfc3339();

        // Find stale sessions
        let stale_keys: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT session_key FROM session_metadata WHERE last_activity < ?1
                     AND NOT EXISTS (
                         SELECT 1 FROM bridge_sources JOIN bridge_inputs USING(source_key)
                         WHERE bridge_sources.session_key = session_metadata.session_key
                           AND bridge_inputs.state IN ('pending', 'running', 'steered', 'outcome_unknown')
                     )",
                )
                .map_err(std::io::Error::other)?;
            let rows = stmt
                .query_map(params![cutoff], |row| row.get(0))
                .map_err(std::io::Error::other)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(std::io::Error::other)?
        };

        let count = stale_keys.len();
        for key in &stale_keys {
            tx.execute("DELETE FROM sessions WHERE session_key = ?1", params![key])
                .map_err(std::io::Error::other)?;
            tx.execute(
                "DELETE FROM session_metadata WHERE session_key = ?1",
                params![key],
            )
            .map_err(std::io::Error::other)?;
            tx.execute(
                "DELETE FROM session_requests WHERE session_key = ?1",
                params![key],
            )
            .map_err(std::io::Error::other)?;
            bridge_intake::redact_session_inputs(&tx, key)?;
        }

        // Receipts are keyed by client-supplied session ids and can outlive
        // (or never have) a metadata row; the per-key cap in `record_request`
        // does not bound them across keys. Sweep receipts whose session no
        // longer exists once they are older than the TTL. Recent orphans stay:
        // a request can be recorded before the session's first append.
        tx.execute(
            "DELETE FROM session_requests
             WHERE updated_at < ?1
               AND session_key NOT IN (SELECT session_key FROM session_metadata)",
            params![cutoff],
        )
        .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(count)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;

        tx.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        let count = tx.changes() as usize;

        if count > 0 {
            tx.execute(
                "UPDATE session_metadata SET message_count = 0, last_activity = ?1 WHERE session_key = ?2",
                params![Utc::now().to_rfc3339(), session_key],
            )
            .map_err(std::io::Error::other)?;
        }
        bridge_intake::redact_session_inputs(&tx, session_key)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(count)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;

        // Check if session exists
        let exists: bool = tx
            .query_row(
                "SELECT COUNT(*) > 0 FROM session_metadata WHERE session_key = ?1",
                params![session_key],
                |row| row.get(0),
            )
            .map_err(std::io::Error::other)?;

        // Delete messages (FTS5 trigger handles sessions_fts cleanup)
        tx.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        // Delete metadata
        tx.execute(
            "DELETE FROM session_metadata WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        tx.execute(
            "DELETE FROM session_requests WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;
        bridge_intake::redact_session_inputs(&tx, session_key)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(exists)
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
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

    fn bridge_resume(
        &self,
        source: &zeroclaw_api::bridge_intake::BridgeSource,
    ) -> std::io::Result<zeroclaw_api::bridge_intake::BridgeResume> {
        self.bridge_resume_impl(source)
    }

    fn bridge_receipt(
        &self,
        source: &zeroclaw_api::bridge_intake::BridgeSource,
        update_id: i64,
    ) -> std::io::Result<Option<zeroclaw_api::bridge_intake::BridgeReceipt>> {
        self.bridge_receipt_impl(source, update_id)
    }

    fn bridge_record(
        &self,
        source: &zeroclaw_api::bridge_intake::BridgeSource,
        input: &zeroclaw_api::bridge_intake::BridgeInput,
    ) -> std::io::Result<zeroclaw_api::bridge_intake::BridgeReceipt> {
        self.bridge_record_impl(source, input)
    }

    fn bridge_claim(
        &self,
        source: &zeroclaw_api::bridge_intake::BridgeSource,
        update_id: i64,
    ) -> std::io::Result<bool> {
        self.bridge_claim_impl(source, update_id)
    }

    fn bridge_finish(
        &self,
        source: &zeroclaw_api::bridge_intake::BridgeSource,
        update_id: i64,
        state: &str,
    ) -> std::io::Result<()> {
        self.bridge_finish_impl(source, update_id, state)
    }

    fn record_request(
        &self,
        session_key: &str,
        request_id: &str,
        state: &str,
    ) -> std::io::Result<Option<RequestReceipt>> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(std::io::Error::other)?;
        let known = tx.query_row(
            "SELECT state FROM session_requests WHERE session_key = ?1 AND request_id = ?2",
            params![session_key, request_id],
            |row| row.get::<_, String>(0),
        );
        match known {
            Ok(state) => return Ok(Some(RequestReceipt::Duplicate { state })),
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(std::io::Error::other(e)),
        }
        let cutoff = (Utc::now()
            - Duration::minutes(i64::from(REQUEST_RECEIPT_PROTECTION_MINUTES)))
        .to_rfc3339();
        let per_session: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM session_requests WHERE session_key = ?1",
                params![session_key],
                |r| r.get(0),
            )
            .map_err(std::io::Error::other)?;
        if per_session >= REQUEST_RECEIPTS_PER_SESSION {
            tx.execute(
                "DELETE FROM session_requests WHERE session_key = ?1 AND accepted_at < ?2",
                params![session_key, cutoff],
            )
            .map_err(std::io::Error::other)?;
        }
        let global: i64 = tx
            .query_row("SELECT COUNT(*) FROM session_requests", [], |r| r.get(0))
            .map_err(std::io::Error::other)?;
        if global >= REQUEST_RECEIPTS_GLOBAL_CAP {
            tx.execute(
                "DELETE FROM session_requests WHERE accepted_at < ?1",
                params![cutoff],
            )
            .map_err(std::io::Error::other)?;
        }
        let (per_session, global): (i64, i64) = tx.query_row(
            "SELECT (SELECT COUNT(*) FROM session_requests WHERE session_key = ?1), (SELECT COUNT(*) FROM session_requests)",
            params![session_key], |r| Ok((r.get(0)?, r.get(1)?))).map_err(std::io::Error::other)?;
        if per_session >= REQUEST_RECEIPTS_PER_SESSION || global >= REQUEST_RECEIPTS_GLOBAL_CAP {
            return Err(std::io::Error::other(
                "protected request receipt capacity exhausted",
            ));
        }
        let now = Utc::now().to_rfc3339();
        tx.execute("INSERT INTO session_requests (session_key, request_id, state, accepted_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)", params![session_key, request_id, state, now]).map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        Ok(Some(RequestReceipt::Recorded))
    }

    fn set_request_state(
        &self,
        session_key: &str,
        request_id: &str,
        state: &str,
    ) -> std::io::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE session_requests SET state = ?1, updated_at = ?2
             WHERE session_key = ?3 AND request_id = ?4",
            params![state, Utc::now().to_rfc3339(), session_key, request_id],
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
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
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
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
                    "SELECT created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id FROM session_metadata WHERE session_key = ?1",
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The daemon's gateway, channels and bridge intake open the store
    /// together when it starts. On a fresh data dir every one of them must
    /// get a working store, not a lost `ALTER` race or a busy journal-mode
    /// switch.
    #[test]
    fn concurrent_first_openers_of_a_fresh_store_all_succeed() {
        const TRIALS: usize = 40;
        const OPENERS: usize = 4;
        for trial in 0..TRIALS {
            let tmp = TempDir::new().unwrap();
            let start = std::sync::Arc::new(std::sync::Barrier::new(OPENERS));
            let openers: Vec<_> = (0..OPENERS)
                .map(|opener| {
                    let (dir, start) = (tmp.path().to_path_buf(), std::sync::Arc::clone(&start));
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
    fn cleanup_stale_sweeps_request_receipts() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let old_time = (Utc::now() - Duration::hours(100)).to_rfc3339();

        // A stale session with a receipt, and an old orphaned receipt whose
        // session never got a metadata row.
        {
            let conn = backend.conn.lock();
            conn.execute(
                "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count) VALUES (?1, ?2, ?2, 0)",
                params!["stale", old_time],
            ).unwrap();
            for key in ["stale", "orphan_old"] {
                conn.execute(
                    "INSERT INTO session_requests (session_key, request_id, state, accepted_at, updated_at) VALUES (?1, 'r1', 'done', ?2, ?2)",
                    params![key, old_time],
                ).unwrap();
            }
        }
        // A live session's receipt and a fresh orphan (recorded before the
        // session's first append) must survive.
        backend.append("live", &ChatMessage::user("hi")).unwrap();
        backend.record_request("live", "r1", "accepted").unwrap();
        backend
            .record_request("orphan_new", "r1", "accepted")
            .unwrap();

        assert_eq!(backend.cleanup_stale(48).unwrap(), 1);

        let remaining: Vec<String> = {
            let conn = backend.conn.lock();
            let mut stmt = conn
                .prepare("SELECT session_key FROM session_requests ORDER BY session_key")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(
            remaining,
            vec!["live".to_string(), "orphan_new".to_string()]
        );
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
    fn request_receipts_record_once_and_report_duplicates_with_state() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        assert_eq!(
            backend.record_request("s1", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Recorded)
        );
        // The same id on another session is a different request.
        assert_eq!(
            backend.record_request("s2", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Recorded)
        );
        backend.set_request_state("s1", "r1", "done").unwrap();
        assert_eq!(
            backend.record_request("s1", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Duplicate {
                state: "done".into()
            })
        );

        // Receipts survive reopening the database.
        drop(backend);
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(matches!(
            backend.record_request("s1", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Duplicate { .. })
        ));
    }

    #[test]
    fn request_receipts_are_bounded_and_deleted_with_the_session() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        for i in 0..REQUEST_RECEIPTS_PER_SESSION {
            backend
                .record_request("s1", &format!("r{i}"), "accepted")
                .unwrap();
        }
        assert!(
            backend
                .record_request("s1", "overflow", "accepted")
                .is_err()
        );
        assert_eq!(
            backend.record_request("s1", "r0", "accepted").unwrap(),
            Some(RequestReceipt::Duplicate {
                state: "accepted".into()
            })
        );
        // Capacity may reclaim receipts only after the protected retry window.
        let old = (Utc::now()
            - Duration::minutes(i64::from(REQUEST_RECEIPT_PROTECTION_MINUTES) + 1))
        .to_rfc3339();
        backend
            .conn
            .lock()
            .execute(
                "UPDATE session_requests SET accepted_at = ?1 WHERE request_id = 'r0'",
                params![old],
            )
            .unwrap();
        backend.set_request_state("s1", "r0", "done").unwrap();
        assert_eq!(
            backend
                .record_request("s1", "overflow", "accepted")
                .unwrap(),
            Some(RequestReceipt::Recorded)
        );
        assert!(matches!(
            backend.record_request("s1", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Duplicate { .. })
        ));
        assert!(backend.delete_session("s1").unwrap());
        assert_eq!(
            backend.record_request("s1", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Recorded)
        );
    }

    #[test]
    fn request_receipts_are_globally_bounded_across_sessions() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        for i in 0..REQUEST_RECEIPTS_GLOBAL_CAP {
            backend
                .record_request(&format!("gw_{i}"), "r1", "accepted")
                .unwrap();
        }
        for i in 0..200 {
            assert!(
                backend
                    .record_request(&format!("overflow_{i}"), "r1", "accepted")
                    .is_err()
            );
        }
        let rows: i64 = backend
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM session_requests", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, REQUEST_RECEIPTS_GLOBAL_CAP);
        assert!(matches!(
            backend.record_request("gw_0", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Duplicate { .. })
        ));
        let old = (Utc::now()
            - Duration::minutes(i64::from(REQUEST_RECEIPT_PROTECTION_MINUTES) + 1))
        .to_rfc3339();
        backend
            .conn
            .lock()
            .execute(
                "UPDATE session_requests SET accepted_at = ?1 WHERE session_key = 'gw_0'",
                params![old],
            )
            .unwrap();
        assert_eq!(
            backend.record_request("new", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Recorded)
        );
        assert!(matches!(
            backend.record_request("gw_1", "r1", "accepted").unwrap(),
            Some(RequestReceipt::Duplicate { .. })
        ));
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
}
