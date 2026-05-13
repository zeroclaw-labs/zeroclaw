//! SQLite-backed session persistence with FTS5 search.
//!
//! Stores sessions in `{workspace}/sessions/sessions.db` using WAL mode.
//! Provides full-text search via FTS5 and automatic TTL-based cleanup.
//! Designed as the default backend, replacing JSONL for new installations.

use crate::session_backend::{SessionBackend, SessionMetadata, SessionQuery, SessionState};
use crate::slot::{Slot, SlotState, SlotStore, SlotUpdate};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use zeroclaw_api::provider::ChatMessage;

/// SQLite-backed session store with FTS5 and WAL mode.
pub struct SqliteSessionBackend {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
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
             PRAGMA mmap_size = 4194304;",
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

        // Migration: add name column to existing databases
        let has_name: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') WHERE name = 'name'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_name {
            let _ = conn.execute("ALTER TABLE session_metadata ADD COLUMN name TEXT", []);
        }

        // Migration: add state tracking columns
        let has_state: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') WHERE name = 'state'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_state {
            let _ = conn.execute(
                "ALTER TABLE session_metadata ADD COLUMN state TEXT NOT NULL DEFAULT 'idle'",
                [],
            );
            let _ = conn.execute("ALTER TABLE session_metadata ADD COLUMN turn_id TEXT", []);
            let _ = conn.execute(
                "ALTER TABLE session_metadata ADD COLUMN turn_started_at TEXT",
                [],
            );
        }

        // Migration: add slots table (M1). Additive — pre-existing session
        // data remains untouched. `agent_config` is stored as JSON text so
        // future fields can be added without a schema bump.
        //
        // No FK on session_key — slots can refer to memory sessions that
        // don't exist yet when a slot is created ahead of its first turn.
        // Cleanup of orphaned memory sessions is the caller's concern.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS slots (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL,
                title           TEXT NOT NULL,
                agent_config    TEXT NOT NULL DEFAULT '{}',
                state           TEXT NOT NULL DEFAULT 'idle',
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL,
                message_count   INTEGER NOT NULL DEFAULT 0,
                dirty           INTEGER NOT NULL DEFAULT 0,
                workspace       TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_slots_session_id ON slots(session_id);
             CREATE INDEX IF NOT EXISTS idx_slots_updated_at ON slots(updated_at DESC);",
        )
        .context("Failed to initialize slots schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
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
            "SELECT session_key, created_at, last_activity, message_count, name
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
            "SELECT session_key, created_at, last_activity, message_count, name
             FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| {
                let key: String = row.get(0)?;
                let created_str: String = row.get(1)?;
                let activity_str: String = row.get(2)?;
                let count: i64 = row.get(3)?;
                let name: Option<String> = row.get(4)?;

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
            "SELECT session_key, created_at, last_activity, message_count, name
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
            "SELECT session_key, created_at, last_activity, message_count, name
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
                    "SELECT created_at, last_activity, message_count, name FROM session_metadata WHERE session_key = ?1",
                    params![key],
                    |row| {
                        let created_str: String = row.get(0)?;
                        let activity_str: String = row.get(1)?;
                        let count: i64 = row.get(2)?;
                        let name: Option<String> = row.get(3)?;
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
                        })
                    },
                )
                .ok()
            })
            .collect()
    }
}

// ── SlotStore (M1: multi-session dashboard) ─────────────────────────────

fn slot_state_to_str(state: SlotState) -> &'static str {
    match state {
        SlotState::Idle => "idle",
        SlotState::Running => "running",
        SlotState::WaitingApproval => "waiting_approval",
        SlotState::Error => "error",
    }
}

fn slot_state_from_str(s: &str) -> SlotState {
    match s {
        "running" => SlotState::Running,
        "waiting_approval" => SlotState::WaitingApproval,
        "error" => SlotState::Error,
        _ => SlotState::Idle,
    }
}

fn row_to_slot(row: &rusqlite::Row<'_>) -> rusqlite::Result<Slot> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let title: String = row.get(2)?;
    let agent_config_json: String = row.get(3)?;
    let state_str: String = row.get(4)?;
    let created_at: i64 = row.get(5)?;
    let updated_at: i64 = row.get(6)?;
    let message_count: i64 = row.get(7)?;
    let dirty_int: i64 = row.get(8)?;
    let workspace: Option<String> = row.get(9)?;

    let agent_config = serde_json::from_str(&agent_config_json).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to deserialize agent_config for slot {id}: {err}"),
            )),
        )
    })?;

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    Ok(Slot {
        id,
        session_id,
        title,
        agent_config,
        state: slot_state_from_str(&state_str),
        created_at,
        updated_at,
        message_count: message_count.max(0) as usize,
        dirty: dirty_int != 0,
        workspace,
    })
}

impl SlotStore for SqliteSessionBackend {
    fn create_slot(&self, slot: &Slot) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let agent_config_json =
            serde_json::to_string(&slot.agent_config).map_err(std::io::Error::other)?;
        #[allow(clippy::cast_possible_wrap)]
        let message_count = slot.message_count as i64;

        conn.execute(
            "INSERT INTO slots
                (id, session_id, title, agent_config, state,
                 created_at, updated_at, message_count, dirty, workspace)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                slot.id,
                slot.session_id,
                slot.title,
                agent_config_json,
                slot_state_to_str(slot.state),
                slot.created_at,
                slot.updated_at,
                message_count,
                i32::from(slot.dirty),
                slot.workspace,
            ],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_slot(&self, slot_id: &str) -> std::io::Result<Option<Slot>> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT id, session_id, title, agent_config, state,
                    created_at, updated_at, message_count, dirty, workspace
             FROM slots WHERE id = ?1",
            params![slot_id],
            row_to_slot,
        );
        match row {
            Ok(slot) => Ok(Some(slot)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(std::io::Error::other(e)),
        }
    }

    fn list_slots(&self) -> std::io::Result<Vec<Slot>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, title, agent_config, state,
                        created_at, updated_at, message_count, dirty, workspace
                 FROM slots ORDER BY updated_at DESC, id ASC",
            )
            .map_err(std::io::Error::other)?;

        let rows = stmt
            .query_map([], row_to_slot)
            .map_err(std::io::Error::other)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(std::io::Error::other)?);
        }
        Ok(out)
    }

    fn update_slot(&self, slot_id: &str, update: &SlotUpdate) -> std::io::Result<Option<Slot>> {
        let now = Utc::now().timestamp();
        let conn = self.conn.lock();

        // Fetch the existing slot so unspecified fields keep their values.
        // Use a single SELECT then UPDATE rather than N per-field updates to
        // keep the operation atomic-enough under the connection mutex.
        let existing_row = conn.query_row(
            "SELECT id, session_id, title, agent_config, state,
                    created_at, updated_at, message_count, dirty, workspace
             FROM slots WHERE id = ?1",
            params![slot_id],
            row_to_slot,
        );
        let mut existing = match existing_row {
            Ok(s) => s,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(std::io::Error::other(e)),
        };

        if let Some(t) = &update.title {
            existing.title = t.clone();
        }
        if let Some(cfg) = &update.agent_config {
            existing.agent_config = cfg.clone();
        }
        if let Some(state) = update.state {
            existing.state = state;
        }
        if let Some(ws) = &update.workspace {
            existing.workspace = Some(ws.clone());
        }
        if let Some(dirty) = update.dirty {
            existing.dirty = dirty;
        }
        if let Some(count) = update.message_count {
            existing.message_count = count;
        }
        existing.updated_at = now;

        let agent_config_json =
            serde_json::to_string(&existing.agent_config).map_err(std::io::Error::other)?;
        #[allow(clippy::cast_possible_wrap)]
        let message_count = existing.message_count as i64;

        conn.execute(
            "UPDATE slots SET
                title = ?1, agent_config = ?2, state = ?3,
                updated_at = ?4, message_count = ?5, dirty = ?6, workspace = ?7
             WHERE id = ?8",
            params![
                existing.title,
                agent_config_json,
                slot_state_to_str(existing.state),
                existing.updated_at,
                message_count,
                i32::from(existing.dirty),
                existing.workspace,
                slot_id,
            ],
        )
        .map_err(std::io::Error::other)?;

        Ok(Some(existing))
    }

    fn delete_slot(&self, slot_id: &str) -> std::io::Result<bool> {
        let conn = self.conn.lock();
        let rows = conn
            .execute("DELETE FROM slots WHERE id = ?1", params![slot_id])
            .map_err(std::io::Error::other)?;
        Ok(rows > 0)
    }

    fn count_slots(&self) -> std::io::Result<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM slots", [], |row| row.get(0))
            .map_err(std::io::Error::other)?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Ok(count.max(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    // ── SlotStore tests ───────────────────────────────────────────

    fn make_slot(id: &str) -> Slot {
        let now = Utc::now().timestamp();
        Slot::new(id.into(), format!("gw_{id}"), format!("Slot {id}"), now)
    }

    #[test]
    fn slot_create_and_get_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let slot = make_slot("alpha");
        backend.create_slot(&slot).unwrap();
        let loaded = backend.get_slot("alpha").unwrap().expect("slot exists");
        assert_eq!(loaded, slot);
    }

    #[test]
    fn slot_get_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(backend.get_slot("no-such").unwrap().is_none());
    }

    #[test]
    fn slot_list_sorts_by_updated_at_desc() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let mut a = make_slot("a");
        a.updated_at = 100;
        let mut b = make_slot("b");
        b.updated_at = 200;
        let mut c = make_slot("c");
        c.updated_at = 150;

        backend.create_slot(&a).unwrap();
        backend.create_slot(&b).unwrap();
        backend.create_slot(&c).unwrap();

        let slots = backend.list_slots().unwrap();
        let ids: Vec<_> = slots.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn slot_update_patches_only_requested_fields() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let mut slot = make_slot("x");
        slot.title = "Original".into();
        backend.create_slot(&slot).unwrap();

        let updated = backend
            .update_slot(
                "x",
                &SlotUpdate {
                    title: Some("Renamed".into()),
                    ..SlotUpdate::default()
                },
            )
            .unwrap()
            .expect("slot exists");
        assert_eq!(updated.title, "Renamed");
        assert_eq!(updated.session_id, slot.session_id); // unchanged
        assert!(updated.updated_at >= slot.updated_at);
    }

    #[test]
    fn slot_update_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let res = backend
            .update_slot(
                "nope",
                &SlotUpdate {
                    title: Some("hi".into()),
                    ..SlotUpdate::default()
                },
            )
            .unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn slot_delete_removes_and_reports() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.create_slot(&make_slot("a")).unwrap();
        backend.create_slot(&make_slot("b")).unwrap();

        assert!(backend.delete_slot("a").unwrap());
        assert!(!backend.delete_slot("a").unwrap()); // second delete is a no-op
        let remaining = backend.list_slots().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "b");
    }

    #[test]
    fn slot_count_tracks_inserts_and_deletes() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.count_slots().unwrap(), 0);
        for i in 0..5 {
            backend.create_slot(&make_slot(&format!("s{i}"))).unwrap();
        }
        assert_eq!(backend.count_slots().unwrap(), 5);
        backend.delete_slot("s2").unwrap();
        assert_eq!(backend.count_slots().unwrap(), 4);
    }

    #[test]
    fn slot_agent_config_serializes_round_trip() {
        use crate::slot::{SlotAgentConfig, SlotMode};
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let mut slot = make_slot("cfg");
        slot.agent_config = SlotAgentConfig {
            provider: Some("anthropic".into()),
            model: Some("claude-3-5-sonnet".into()),
            mode: SlotMode::Trust,
            personality: Some("SOUL.md".into()),
            persona_preset: Some("codex-researcher".into()),
        };
        backend.create_slot(&slot).unwrap();
        let loaded = backend.get_slot("cfg").unwrap().unwrap();
        assert_eq!(loaded.agent_config, slot.agent_config);
    }

    #[test]
    fn slot_state_roundtrips_through_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        for state in [
            SlotState::Idle,
            SlotState::Running,
            SlotState::WaitingApproval,
            SlotState::Error,
        ] {
            let mut slot = make_slot(&format!("{state:?}").to_lowercase());
            slot.state = state;
            backend.create_slot(&slot).unwrap();
            let loaded = backend.get_slot(&slot.id).unwrap().unwrap();
            assert_eq!(loaded.state, state, "state {state:?} failed to roundtrip");
        }
    }

    #[test]
    fn slot_migration_is_idempotent_on_reopen() {
        // Opening the backend twice in the same workspace must not fail the
        // CREATE TABLE IF NOT EXISTS and must preserve slot data between
        // opens.
        let tmp = TempDir::new().unwrap();
        {
            let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
            backend.create_slot(&make_slot("keep-me")).unwrap();
        }
        let reopened = SqliteSessionBackend::new(tmp.path()).unwrap();
        let slot = reopened.get_slot("keep-me").unwrap();
        assert!(slot.is_some(), "slot data must survive reopen");
    }

    #[test]
    fn slot_migration_preserves_pre_existing_session_data() {
        // Before M1, backends only stored sessions. M1 must not destroy or
        // corrupt session data. Populate sessions, then add a slot, then
        // confirm sessions still load.
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend
            .append("legacy", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .append("legacy", &ChatMessage::assistant("hi"))
            .unwrap();

        backend.create_slot(&make_slot("new-slot")).unwrap();

        let msgs = backend.load("legacy");
        assert_eq!(msgs.len(), 2, "session data must survive slot migration");
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn slot_agent_config_none_fields_serialize_compactly() {
        // Slots with all-default agent config should still load correctly
        // and present empty overrides (None everywhere) to callers.
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let slot = make_slot("empty-cfg");
        backend.create_slot(&slot).unwrap();
        let loaded = backend.get_slot("empty-cfg").unwrap().unwrap();
        assert!(loaded.agent_config.provider.is_none());
        assert!(loaded.agent_config.model.is_none());
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
