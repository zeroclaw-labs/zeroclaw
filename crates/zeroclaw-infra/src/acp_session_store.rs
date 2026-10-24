//! ACP session persistence.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::Path;
use zeroclaw_api::model_provider::{
    ChatMessage, ConversationMessage, ToolCall, ToolResultMessage, projected_entry_count,
};
use zeroclaw_api::plan::PlanEntry;
use zeroclaw_log::{Action, EventOutcome};

const MAX_PERSISTED_TOOL_OUTPUT_BYTES: usize = 16 * 1024;

/// Internal discriminator for `acp_tool_calls.event_kind`. The 'in' row
/// records the call args; the 'out' row records the result. Two append-only
/// rows per call, correlated by the provider-issued `tool_call_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolEventKind {
    In,
    Out,
}

impl ToolEventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "in" => Some(Self::In),
            "out" => Some(Self::Out),
            _ => None,
        }
    }
}

/// Open-call balance after replaying one tool-call id's rows in order. An
/// `in` opens one call; an `out` folds into an open call while the balance
/// is positive and otherwise is an orphan entry that leaves the balance
/// untouched, so an orphan cannot consume a later call that reuses the id.
fn open_calls_after(events: impl IntoIterator<Item = ToolEventKind>) -> usize {
    let mut open = 0usize;
    for event in events {
        match event {
            ToolEventKind::In => open += 1,
            ToolEventKind::Out => open = open.saturating_sub(1),
        }
    }
    open
}

/// Canonical breadcrumb text, duplicated from
/// `zeroclaw_runtime::agent::history::HISTORY_TRIM_BREADCRUMB_CANONICAL`.
/// This crate sits below `zeroclaw-runtime` in the dependency graph and
/// cannot import it; used only for one-time legacy-row migration below.
/// Keep in sync with the runtime constant.
const HISTORY_TRIM_BREADCRUMB_CANONICAL: &str = "[earlier turns omitted to fit the context window]";
const SYNTHETIC_INTERRUPTION_ROLE: &str = "__zeroclaw_turn_stream_interrupted__";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RetainedContextRecord {
    messages: Vec<ConversationMessage>,
}

/// `(id, role, content, reasoning_content, created_at)` of one `acp_messages` row.
type MessageRow = (i64, String, String, Option<String>, Option<String>);

pub struct AcpSessionStore {
    conn: Mutex<Connection>,
}

pub struct AcpSessionData {
    pub session_uuid: String,
    /// Owning principal (RFC 7141 session isolation). `None` marks an
    /// unscoped/legacy row, visible only to unscoped connections.
    pub principal_id: Option<String>,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub interaction_surface: Option<String>,
    pub token_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<ConversationMessage>,
    /// RFC 3339 `created_at` of the row each entry of `messages` came from,
    /// index-aligned with `messages`. Every message of one turn shares the
    /// turn's finalization time, except the turn's prompt, which carries the
    /// time the turn began.
    pub message_created_at: Vec<Option<String>>,
    /// Whether `messages`' first non-system entry is the synthetic
    /// history-trim breadcrumb. Rows written after the `trim_breadcrumb`
    /// column was added carry this as recorded by the owning turn loop,
    /// never inferred from text. A row from before that column existed has
    /// no recorded value (`NULL`); for that one-time legacy case only, it is
    /// inferred from the first non-system message's text, matching the
    /// interactive-session JSONL migration contract (see
    /// `zeroclaw_runtime::agent::history::load_interactive_session_history_with_crumb`).
    pub trim_breadcrumb: bool,
    /// Provider-facing retained context written by native RPC trims. `None`
    /// preserves the legacy provider-safe replay path.
    pub retained_context: Option<Vec<ConversationMessage>>,
}

pub enum AcpSessionRestore {
    Missing,
    Killed,
    Restorable(Box<AcpSessionData>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpSessionKillTransition {
    Marked,
    AlreadyKilled,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpSessionAccess {
    Owned,
    Foreign,
    Missing,
}

/// Lightweight summary for the ACP session picker. Avoids loading the full
/// message history just to render a one-line label per session.
pub struct AcpSessionSummary {
    pub session_uuid: String,
    /// Owning principal (RFC 7141). `None` = unscoped/legacy row.
    pub principal_id: Option<String>,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub token_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_count: usize,
}

/// The sessions one projected-count refresh pass may score: the scope of
/// the listing about to run, or the one session a count read names. Scoring
/// stays scoped to the rows that listing returns, before any caller-side
/// principal filter, so an agent-scoped listing never rescores the sessions
/// of other agents.
enum ProjectedCountScope<'a> {
    /// The live sessions of every agent (the unscoped picker listing).
    Live,
    /// Every session of one agent, live or killed (the export listing).
    Agent(&'a str),
    /// The live sessions of one agent (the live discovery listing).
    LiveAgent(&'a str),
    /// One session, by uuid (the persisted count getter).
    Session(&'a str),
}

impl ProjectedCountScope<'_> {
    /// The `acp_sessions` predicate that narrows a refresh pass to this
    /// scope, as a `WHERE` tail plus the one value it binds.
    fn session_filter(&self) -> (&'static str, Option<&str>) {
        match *self {
            Self::Live => (" AND s.killed_at IS NULL", None),
            Self::Agent(agent) => (" AND s.agent_alias = ?1", Some(agent)),
            Self::LiveAgent(agent) => (
                " AND s.agent_alias = ?1 AND s.killed_at IS NULL",
                Some(agent),
            ),
            Self::Session(session_uuid) => (" AND s.session_uuid = ?1", Some(session_uuid)),
        }
    }
}

/// A bounded, store-owned page of the typed ACP transcript. The cursor is
/// deliberately opaque to RPC clients; callers must hand it back unchanged.
#[derive(Debug)]
pub struct AcpSessionPage {
    /// Owner read in the same transaction as the page, for RPC authorization
    /// revalidation without loading the complete transcript.
    pub principal_id: Option<String>,
    pub messages: Vec<ConversationMessage>,
    /// Row `created_at` per entry of `messages`, index-aligned with it.
    pub message_created_at: Vec<Option<String>>,
    pub next_cursor: Option<String>,
    pub has_older: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct AcpSessionCursor {
    version: u8,
    session_id: i64,
    max_message_id: i64,
    next_message_id: i64,
    next_entry_offset: Option<usize>,
}

const ACP_SESSION_CURSOR_VERSION: u8 = 1;
const ACP_SESSION_MAX_PAGE_SIZE: usize = 1_000;

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
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;",
        )
        .context("Failed to configure ACP session DB pragmas")?;

        // Schema is create-if-missing: ACP sessions are long-lived user data
        // and must survive daemon restarts. Never drop existing tables here.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS acp_sessions (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_uuid  TEXT NOT NULL UNIQUE,
                 agent_alias   TEXT NOT NULL,
                 workspace_dir TEXT NOT NULL,
                 interaction_surface TEXT,
                 token_count   INTEGER NOT NULL DEFAULT 0,
                 killed_at     TEXT,
                 retained_context_json TEXT,
                 retained_context_frontier INTEGER,
                 created_at    TEXT NOT NULL,
                 last_activity TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acp_sessions_uuid  ON acp_sessions(session_uuid);
             CREATE INDEX IF NOT EXISTS idx_acp_sessions_alias ON acp_sessions(agent_alias);

             CREATE TABLE IF NOT EXISTS acp_messages (
                 id                INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id        INTEGER NOT NULL REFERENCES acp_sessions(id) ON DELETE CASCADE,
                 role              TEXT NOT NULL,
                 content           TEXT NOT NULL,
                 reasoning_content TEXT,
                 created_at        TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acp_messages_session ON acp_messages(session_id, id);

             CREATE TABLE IF NOT EXISTS acp_tool_calls (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 message_id   INTEGER NOT NULL REFERENCES acp_messages(id) ON DELETE CASCADE,
                 tool_call_id TEXT NOT NULL,
                 tool_name    TEXT NOT NULL,
                 event_kind   TEXT NOT NULL,
                 payload      TEXT NOT NULL,
                 outcome      TEXT,
                 created_at   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acp_tool_calls_message ON acp_tool_calls(message_id, id);
             CREATE INDEX IF NOT EXISTS idx_acp_tool_calls_lookup  ON acp_tool_calls(tool_call_id);

             CREATE TABLE IF NOT EXISTS acp_session_events (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id INTEGER NOT NULL REFERENCES acp_sessions(id) ON DELETE CASCADE,
                 action     TEXT NOT NULL,
                 outcome    TEXT NOT NULL,
                 payload    TEXT,
                 created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acp_session_events_session ON acp_session_events(session_id, id);

             CREATE TABLE IF NOT EXISTS acp_turn_checkpoints (
                 session_id    INTEGER PRIMARY KEY REFERENCES acp_sessions(id) ON DELETE CASCADE,
                 turn_id       TEXT NOT NULL,
                 started_at    TEXT
             );

             CREATE TABLE IF NOT EXISTS acp_turn_checkpoint_events (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id INTEGER NOT NULL REFERENCES acp_turn_checkpoints(session_id) ON DELETE CASCADE,
                 payload    TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acp_turn_checkpoint_events_session
                 ON acp_turn_checkpoint_events(session_id, id);",
        )
        .context("Failed to create ACP session schema")?;

        Self::ensure_killed_at_column(&conn)
            .context("Failed to migrate ACP session killed marker")?;

        Self::ensure_plan_json_column(&conn)
            .context("Failed to migrate ACP session plan column")?;

        Self::ensure_interaction_surface_column(&conn)
            .context("Failed to migrate ACP session interaction surface")?;

        Self::ensure_projected_message_count_columns(&conn)
            .context("Failed to migrate ACP session projected message count")?;
        Self::ensure_trim_breadcrumb_column(&conn)
            .context("Failed to migrate ACP session trim breadcrumb column")?;
        Self::ensure_principal_id_column(&conn)
            .context("Failed to migrate ACP session principal owner")?;

        Self::ensure_retained_context_columns(&conn)
            .context("Failed to migrate ACP retained context columns")?;
        Self::ensure_turn_checkpoint_started_at_column(&conn)
            .context("Failed to migrate ACP turn checkpoint start time")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Add the `principal_id` owner column on upgrade (RFC 7141 session
    /// isolation). Existing rows keep a NULL owner -- visible only to unscoped
    /// connections -- mirroring the unified `session_backend` model.
    fn ensure_principal_id_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_sessions)")
            .context("Failed to inspect ACP session schema")?;
        let mut rows = stmt
            .query([])
            .context("Failed to read ACP session schema")?;
        let mut column_present = false;
        while let Some(row) = rows
            .next()
            .context("Failed to read ACP session schema row")?
        {
            let column: String = row
                .get(1)
                .context("Failed to read ACP session column name")?;
            if column == "principal_id" {
                column_present = true;
                break;
            }
        }
        drop(rows);
        drop(stmt);

        // The column and its index are one migration. An interrupted earlier
        // run can leave the column without the index, so the index statement
        // runs whenever the column exists, not only when it was just added.
        if !column_present {
            match conn.execute("ALTER TABLE acp_sessions ADD COLUMN principal_id TEXT", []) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if msg.contains("duplicate column name") => {}
                Err(e) => return Err(e).context("Failed to add ACP session principal owner"),
            }
        }
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_acp_sessions_principal \
             ON acp_sessions(principal_id)",
            [],
        )
        .context("Failed to index ACP session principal owner")?;
        Ok(())
    }

    fn ensure_killed_at_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_sessions)")
            .context("Failed to inspect ACP session schema")?;
        let mut rows = stmt
            .query([])
            .context("Failed to read ACP session schema")?;
        while let Some(row) = rows
            .next()
            .context("Failed to read ACP session schema row")?
        {
            let column: String = row
                .get(1)
                .context("Failed to read ACP session column name")?;
            if column == "killed_at" {
                return Ok(());
            }
        }
        drop(rows);
        drop(stmt);

        match conn.execute("ALTER TABLE acp_sessions ADD COLUMN killed_at TEXT", []) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                if msg.contains("duplicate column name") =>
            {
                Ok(())
            }
            Err(e) => Err(e).context("Failed to add ACP session killed marker"),
        }
    }

    /// Idempotent migration adding the `plan_json` column that stores the
    /// session's latest TodoWrite plan as a JSON array of `PlanEntry`.
    /// Existing user databases predate this column; add it if absent so
    /// the plan survives daemon restarts (durable, like the transcript).
    fn ensure_plan_json_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_sessions)")
            .context("Failed to inspect ACP session schema")?;
        let mut rows = stmt
            .query([])
            .context("Failed to read ACP session schema")?;
        while let Some(row) = rows
            .next()
            .context("Failed to read ACP session schema row")?
        {
            let column: String = row
                .get(1)
                .context("Failed to read ACP session column name")?;
            if column == "plan_json" {
                return Ok(());
            }
        }
        drop(rows);
        drop(stmt);

        match conn.execute("ALTER TABLE acp_sessions ADD COLUMN plan_json TEXT", []) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                if msg.contains("duplicate column name") =>
            {
                Ok(())
            }
            Err(e) => Err(e).context("Failed to add ACP session plan column"),
        }
    }

    /// Idempotent migration for the host-validated interaction surface bound
    /// to the session. NULL identifies sessions created before the field was
    /// introduced or by ACP entry points that do not declare a UI surface.
    fn ensure_interaction_surface_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_sessions)")
            .context("Failed to inspect ACP session schema")?;
        let mut rows = stmt
            .query([])
            .context("Failed to read ACP session schema")?;
        while let Some(row) = rows
            .next()
            .context("Failed to read ACP session schema row")?
        {
            let column: String = row
                .get(1)
                .context("Failed to read ACP session column name")?;
            if column == "interaction_surface" {
                return Ok(());
            }
        }
        drop(rows);
        drop(stmt);

        match conn.execute(
            "ALTER TABLE acp_sessions ADD COLUMN interaction_surface TEXT",
            [],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                if msg.contains("duplicate column name") =>
            {
                Ok(())
            }
            Err(e) => Err(e).context("Failed to add ACP session interaction surface"),
        }
    }

    /// Idempotent migration adding the persisted projected conversation-entry
    /// count the session picker reports, together with the message-id
    /// watermark that says when that count is still valid. The message and
    /// tool-call rows remain the single source of truth: the
    /// (count, watermark) pair on `acp_sessions` is a cache of their
    /// projection, and a cache entry is valid exactly when
    /// `projected_count_through IS (SELECT MAX(id) FROM acp_messages WHERE
    /// session_id = s.id)` (SQLite `IS` is null-safe: an empty session with
    /// a NULL watermark is valid at count 0). Message ids are AUTOINCREMENT,
    /// so every insert by any binary lands past any earlier watermark and
    /// turns the cache stale instead of silently wrong. Stale sessions are
    /// rescored lazily from their rows on the read paths, never at open, so
    /// this migration is two plain idempotent column adds and the
    /// constructor never scores a session. A database an earlier build of
    /// this store already completed its one-pass scoring on needs nothing:
    /// that build's schema stamp is simply unused, and its sessions carry a
    /// NULL watermark and rescore lazily.
    fn ensure_projected_message_count_columns(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_sessions)")
            .context("Failed to inspect ACP session schema")?;
        let mut rows = stmt
            .query([])
            .context("Failed to read ACP session schema")?;
        let mut count_present = false;
        let mut watermark_present = false;
        while let Some(row) = rows
            .next()
            .context("Failed to read ACP session schema row")?
        {
            let column: String = row
                .get(1)
                .context("Failed to read ACP session column name")?;
            if column == "projected_message_count" {
                count_present = true;
            }
            if column == "projected_count_through" {
                watermark_present = true;
            }
        }
        drop(rows);
        drop(stmt);

        if !count_present {
            match conn.execute(
                "ALTER TABLE acp_sessions ADD COLUMN projected_message_count INTEGER NOT NULL DEFAULT 0",
                [],
            ) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if msg.contains("duplicate column name") => {}
                Err(e) => {
                    return Err(e).context("Failed to add ACP session projected message count");
                }
            }
        }
        if !watermark_present {
            match conn.execute(
                "ALTER TABLE acp_sessions ADD COLUMN projected_count_through INTEGER",
                [],
            ) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if msg.contains("duplicate column name") => {}
                Err(e) => {
                    return Err(e).context("Failed to add ACP session projected count watermark");
                }
            }
        }
        Ok(())
    }

    /// Idempotent migration adding the `trim_breadcrumb` column: whether the
    /// persisted transcript's first non-system message is the synthetic
    /// history-trim marker. Stored as one canonical fact alongside the
    /// transcript so a restore never has to infer provenance from message
    /// text (a genuine user turn that happens to equal the localized
    /// breadcrumb string must keep its turn-boundary role).
    fn ensure_trim_breadcrumb_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_sessions)")
            .context("Failed to inspect ACP session schema")?;
        let mut rows = stmt
            .query([])
            .context("Failed to read ACP session schema")?;
        while let Some(row) = rows
            .next()
            .context("Failed to read ACP session schema row")?
        {
            let column: String = row
                .get(1)
                .context("Failed to read ACP session column name")?;
            if column == "trim_breadcrumb" {
                return Ok(());
            }
        }
        drop(rows);
        drop(stmt);

        // No `NOT NULL DEFAULT 0`: a `0` default would be indistinguishable
        // from an explicit "no breadcrumb" recorded by the owning turn loop.
        // Existing rows get `NULL` (unknown/legacy) and are migrated by
        // text inference on load; every row written after this migration
        // gets an explicit 0 or 1.
        match conn.execute(
            "ALTER TABLE acp_sessions ADD COLUMN trim_breadcrumb INTEGER",
            [],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                if msg.contains("duplicate column name") =>
            {
                Ok(())
            }
            Err(e) => Err(e).context("Failed to add ACP session trim breadcrumb column"),
        }
    }

    /// Add the nullable `started_at` column on upgrade. A checkpoint begun by
    /// an older binary has no start time; its prompt row then keeps the
    /// finalization time, as before.
    fn ensure_turn_checkpoint_started_at_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_turn_checkpoints)")
            .context("Failed to inspect ACP turn checkpoint schema")?;
        let present = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .context("Failed to read ACP turn checkpoint schema")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to read ACP turn checkpoint column name")?
            .iter()
            .any(|column| column == "started_at");
        drop(stmt);
        if present {
            return Ok(());
        }
        match conn.execute(
            "ALTER TABLE acp_turn_checkpoints ADD COLUMN started_at TEXT",
            [],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                if msg.contains("duplicate column name") =>
            {
                Ok(())
            }
            Err(e) => Err(e).context("Failed to add started_at"),
        }
    }

    fn ensure_retained_context_columns(conn: &Connection) -> Result<()> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(acp_sessions)")
            .context("Failed to inspect ACP session schema")?;
        let mut rows = stmt
            .query([])
            .context("Failed to read ACP session schema")?;
        let mut columns = std::collections::HashSet::new();
        while let Some(row) = rows
            .next()
            .context("Failed to read ACP session schema row")?
        {
            columns.insert(
                row.get::<_, String>(1)
                    .context("Failed to read ACP session column name")?,
            );
        }
        drop(rows);
        drop(stmt);
        for (name, sql) in [
            (
                "retained_context_json",
                "ALTER TABLE acp_sessions ADD COLUMN retained_context_json TEXT",
            ),
            (
                "retained_context_frontier",
                "ALTER TABLE acp_sessions ADD COLUMN retained_context_frontier INTEGER",
            ),
        ] {
            if columns.contains(name) {
                continue;
            }
            match conn.execute(sql, []) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if msg.contains("duplicate column name") => {}
                Err(e) => return Err(e).with_context(|| format!("Failed to add {name}")),
            }
        }
        Ok(())
    }

    /// One-time legacy migration for rows written before the
    /// `trim_breadcrumb` column existed (`NULL`): infer provenance from
    /// whether the first non-system message is exactly the canonical
    /// breadcrumb text. A genuine user turn that happens to equal that text
    /// is misclassified on this one-time migration only; callers must
    /// persist the result via `record_inferred_trim_breadcrumb` so the
    /// column stops being `NULL` and later restores read the recorded fact
    /// instead of re-inferring it from message text on every load. This
    /// mirrors the JSONL interactive-session migration in
    /// `zeroclaw_runtime::agent::history::load_interactive_session_history_with_crumb`,
    /// restricted to the locale-independent canonical string because this
    /// crate sits below the runtime i18n layer.
    fn infer_legacy_trim_breadcrumb(messages: &[ConversationMessage]) -> bool {
        messages
            .iter()
            .find_map(|m| match m {
                ConversationMessage::Chat(chat) if chat.role != "system" => Some(chat),
                _ => None,
            })
            .is_some_and(|first| {
                first.role == "user" && first.content == HISTORY_TRIM_BREADCRUMB_CANONICAL
            })
    }

    /// Write the one-time `infer_legacy_trim_breadcrumb` result back to the
    /// still-`NULL` column, using the connection the caller already holds
    /// (avoids re-locking `self.conn`, which the caller's `load_session*`
    /// query has open). After this, the column is no longer `NULL` for this
    /// session, so the next restore reads the recorded fact rather than
    /// inferring it again from user-controlled text.
    fn record_inferred_trim_breadcrumb(
        conn: &Connection,
        session_uuid: &str,
        inferred: bool,
    ) -> Result<()> {
        conn.execute(
            "UPDATE acp_sessions SET trim_breadcrumb = ?1 WHERE session_uuid = ?2",
            params![i64::from(inferred), session_uuid],
        )
        .context("Failed to record inferred legacy trim_breadcrumb")?;
        Ok(())
    }

    /// Record a new session stamped with its owning principal (RFC 7141
    /// session isolation). `principal_id` is the caller's scope, or `None` for
    /// an unscoped/admin connection (NULL owner => visible only to unscoped
    /// connections). Returns the integer `id` assigned by SQLite.
    pub fn create_session(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
        principal_id: Option<&str>,
    ) -> Result<i64> {
        self.create_session_with_interaction_surface(
            session_uuid,
            agent_alias,
            workspace_dir,
            None,
            principal_id,
        )
    }

    /// Record a session with an optional host-validated interaction surface,
    /// stamped with its owning principal (RFC 7141).
    pub fn create_session_with_interaction_surface(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
        interaction_surface: Option<&str>,
        principal_id: Option<&str>,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity, trim_breadcrumb, principal_id)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5, 0, ?6)",
            params![
                session_uuid,
                agent_alias,
                workspace_dir,
                interaction_surface,
                now,
                principal_id
            ],
        )
        .context("Failed to create ACP session")?;
        Ok(conn.last_insert_rowid())
    }

    /// Delete a session ONLY if `owner_principal_id` matches the stored
    /// owner, in one predicated statement (RFC 7141 atomic ownership).
    /// Child-row cleanup follows the same cascade as [`Self::delete_session`].
    pub fn delete_session_owned(
        &self,
        session_uuid: &str,
        owner_principal_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "DELETE FROM acp_sessions WHERE session_uuid = ?1 AND principal_id = ?2",
                params![session_uuid, owner_principal_id],
            )
            .context("Failed to delete owned ACP session")?;
        Ok(rows > 0)
    }

    /// The owning principal of a session, for authorization on mutation paths
    /// (RFC 7141 F2) without hydrating its full message history. Returns
    /// `Ok(None)` when the session does not exist, `Ok(Some(None))` for a
    /// NULL-owner (unscoped/legacy) row, and `Ok(Some(Some(id)))` when owned.
    #[allow(clippy::option_option)]
    pub fn session_principal(&self, session_uuid: &str) -> Result<Option<Option<String>>> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT principal_id FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| row.get::<_, Option<String>>(0),
        );
        match row {
            Ok(owner) => Ok(Some(owner)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("Failed to query ACP session owner"),
        }
    }

    /// Read the owner and interaction surface needed before checkpoint
    /// recovery without hydrating a potentially large transcript.
    pub fn session_owner_and_surface(
        &self,
        session_uuid: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT principal_id, interaction_surface FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("Failed to query ACP session owner and interaction surface")
    }

    /// Bind an unlabelled legacy session to a validated surface exactly once.
    /// Returns the durable value after the update so the caller can reject a
    /// concurrent or pre-existing mismatch.
    pub fn bind_interaction_surface_if_unset(
        &self,
        session_uuid: &str,
        interaction_surface: &str,
    ) -> Result<String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE acp_sessions
             SET interaction_surface = ?1
             WHERE session_uuid = ?2 AND interaction_surface IS NULL",
            params![interaction_surface, session_uuid],
        )
        .context("Failed to bind ACP session interaction surface")?;
        conn.query_row(
            "SELECT interaction_surface FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| row.get(0),
        )
        .with_context(|| format!("unknown session_uuid: {session_uuid}"))
    }

    /// Load session metadata and full message history for restore.
    /// Returns `None` if the session_uuid is not found.
    pub fn load_session(&self, session_uuid: &str) -> Result<Option<AcpSessionData>> {
        let conn = self.conn.lock();

        let row = conn.query_row(
            "SELECT id, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity, trim_breadcrumb, principal_id
             FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        );

        let (
            session_id,
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count,
            created_at_s,
            last_activity_s,
            trim_breadcrumb_raw,
            principal_id,
        ) = match row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e).context("Failed to query ACP session"),
        };

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);

        let (messages, message_created_at) = Self::load_messages_with_times(&conn, session_id)?;
        let retained_context = Self::load_retained_context(&conn, session_id)?;
        let trim_breadcrumb = match trim_breadcrumb_raw {
            Some(v) => v != 0,
            None => {
                let inferred = Self::infer_legacy_trim_breadcrumb(&messages);
                Self::record_inferred_trim_breadcrumb(&conn, session_uuid, inferred)?;
                inferred
            }
        };

        Ok(Some(AcpSessionData {
            session_uuid: session_uuid.to_string(),
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
            message_created_at,
            trim_breadcrumb,
            retained_context,
            principal_id,
        }))
    }

    /// Load one bounded page of the projected ACP transcript. Cursor reads
    /// hydrate only the durable groups needed for this page; the cursor's
    /// message bound makes later appends invisible to an established walk.
    pub fn load_message_page(
        &self,
        session_uuid: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<AcpSessionPage> {
        // Cursor validation and page hydration must share one snapshot so a
        // concurrent transcript replacement cannot create false exhaustion.
        let mut conn = self.conn.lock();
        let conn = conn
            .transaction()
            .context("Failed to begin ACP session page read transaction")?;
        let (session_id, principal_id): (i64, Option<String>) = conn
            .query_row(
                "SELECT id, principal_id FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow::Error::msg(format!("unknown ACP session: {session_uuid}")))?;
        let snapshot_max: i64 = conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM acp_messages
             WHERE session_id = ?1 AND role != 'system'",
            params![session_id],
            |row| row.get(0),
        )?;

        let state = match cursor {
            Some(encoded) => decode_cursor(encoded)?,
            None => AcpSessionCursor {
                version: ACP_SESSION_CURSOR_VERSION,
                session_id,
                max_message_id: snapshot_max,
                next_message_id: snapshot_max,
                next_entry_offset: None,
            },
        };
        if state.version != ACP_SESSION_CURSOR_VERSION
            || state.session_id != session_id
            || state.max_message_id < 0
            || state.max_message_id > snapshot_max
            || state.next_message_id < 0
            || state.next_message_id > state.max_message_id
            || state.next_entry_offset == Some(0)
            || (cursor.is_some() && state.next_message_id == 0)
        {
            return Err(anyhow::Error::msg("invalid ACP session cursor"));
        }
        if limit == 0 || limit > ACP_SESSION_MAX_PAGE_SIZE {
            return Err(anyhow::Error::msg(format!(
                "cursor page limit must be between 1 and {ACP_SESSION_MAX_PAGE_SIZE}"
            )));
        }
        if cursor.is_some() {
            let next_row_exists: i64 = conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM acp_messages
                     WHERE session_id = ?1 AND role != 'system' AND id = ?2 AND id <= ?3
                 )",
                params![session_id, state.next_message_id, state.max_message_id],
                |row| row.get(0),
            )?;
            if next_row_exists == 0 {
                return Err(anyhow::Error::msg("invalid ACP session cursor"));
            }
        }
        if state.next_message_id == 0 {
            return Ok(AcpSessionPage {
                principal_id,
                messages: Vec::new(),
                message_created_at: Vec::new(),
                next_cursor: None,
                has_older: false,
            });
        }

        let mut current_id = state.next_message_id;
        let mut end_offset = state.next_entry_offset;
        let mut remaining = limit;
        let mut reverse_page = Vec::new();
        let mut next: Option<AcpSessionCursor> = None;

        while remaining > 0 && current_id > 0 {
            let row = conn
                .query_row(
                    "SELECT id, role, content, reasoning_content, created_at
                     FROM acp_messages
                     WHERE session_id = ?1 AND role != 'system' AND id <= ?2 AND id <= ?3
                     ORDER BY id DESC LIMIT 1",
                    params![session_id, state.max_message_id, current_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((message_id, role, content, reasoning_content, row_created_at)) = row else {
                break;
            };
            let group = load_projected_group(&conn, message_id, role, content, reasoning_content)?;
            let end = end_offset.unwrap_or(group.len());
            if end > group.len() {
                return Err(anyhow::Error::msg("invalid ACP session cursor offset"));
            }
            if group.len() == 0 {
                group.ensure_well_formed(&conn)?;
            }
            let start = end.saturating_sub(remaining);
            reverse_page.push((
                group.entries_to_messages(
                    &conn,
                    start..end,
                    session_id,
                    state.max_message_id,
                    message_id,
                )?,
                row_created_at,
            ));
            remaining -= end - start;

            let previous = if start > 0 {
                next = Some(AcpSessionCursor {
                    version: ACP_SESSION_CURSOR_VERSION,
                    session_id,
                    max_message_id: state.max_message_id,
                    next_message_id: message_id,
                    next_entry_offset: Some(start),
                });
                None
            } else {
                let previous: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM acp_messages
                         WHERE session_id = ?1 AND role != 'system' AND id < ?2 AND id <= ?3
                         ORDER BY id DESC LIMIT 1",
                        params![session_id, message_id, state.max_message_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                next = previous.map(|id| AcpSessionCursor {
                    version: ACP_SESSION_CURSOR_VERSION,
                    session_id,
                    max_message_id: state.max_message_id,
                    next_message_id: id,
                    next_entry_offset: None,
                });
                previous
            };
            if remaining == 0 {
                break;
            }
            let Some(previous) = previous else {
                next = None;
                break;
            };
            current_id = previous;
            end_offset = None;
        }

        let mut messages = Vec::new();
        let mut message_created_at = Vec::new();
        for (group, row_created_at) in reverse_page.into_iter().rev() {
            messages.extend(group);
            message_created_at.resize(messages.len(), row_created_at);
        }
        let next_cursor = next.map(encode_cursor).transpose()?;
        let has_older = next_cursor.is_some();
        Ok(AcpSessionPage {
            principal_id,
            messages,
            message_created_at,
            next_cursor,
            has_older,
        })
    }

    /// Load a durable ACP transcript only when both its UUID and owning agent
    /// match. Keeping the alias predicate in SQL makes an unknown UUID and a
    /// UUID owned by another agent indistinguishable to callers.
    /// Killed sessions remain readable as history; killing prevents runtime
    /// rehydration, not access to retained transcripts by their owner.
    pub fn load_session_for_agent(
        &self,
        session_uuid: &str,
        agent_alias: &str,
    ) -> Result<Option<AcpSessionData>> {
        let conn = self.conn.lock();

        let row = conn
            .query_row(
                "SELECT id, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity, trim_breadcrumb, principal_id
                 FROM acp_sessions
                 WHERE session_uuid = ?1 AND agent_alias = ?2",
                params![session_uuid, agent_alias],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()
            .context("Failed to query ACP session for agent")?;

        let Some((
            session_id,
            owner_alias,
            workspace_dir,
            interaction_surface,
            token_count,
            created_at_s,
            last_activity_s,
            trim_breadcrumb_raw,
            principal_id,
        )) = row
        else {
            return Ok(None);
        };

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);
        let (messages, message_created_at) = Self::load_messages_with_times(&conn, session_id)?;
        let retained_context = Self::load_retained_context(&conn, session_id)?;
        let trim_breadcrumb = match trim_breadcrumb_raw {
            Some(v) => v != 0,
            None => {
                let inferred = Self::infer_legacy_trim_breadcrumb(&messages);
                Self::record_inferred_trim_breadcrumb(&conn, session_uuid, inferred)?;
                inferred
            }
        };

        Ok(Some(AcpSessionData {
            session_uuid: session_uuid.to_string(),
            agent_alias: owner_alias,
            workspace_dir,
            interaction_surface,
            token_count: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
            message_created_at,
            trim_breadcrumb,
            retained_context,
            principal_id,
        }))
    }

    /// Classify an exact ACP session key without exposing the foreign owner.
    /// Callers use `Foreign` and `Missing` to make the same fail-closed response
    /// while still avoiding an unsafe fallback to another session backend.
    pub fn classify_session_for_agent(
        &self,
        session_uuid: &str,
        agent_alias: &str,
    ) -> Result<AcpSessionAccess> {
        let conn = self.conn.lock();
        let owner = conn
            .query_row(
                "SELECT agent_alias FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("Failed to classify ACP session owner")?;
        Ok(match owner {
            Some(owner) if owner == agent_alias => AcpSessionAccess::Owned,
            Some(_) => AcpSessionAccess::Foreign,
            None => AcpSessionAccess::Missing,
        })
    }

    /// Whether `session_uuid` is a live ACP session owned by `agent_alias`.
    pub fn is_live_session_for_agent(&self, session_uuid: &str, agent_alias: &str) -> Result<bool> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM acp_sessions
                 WHERE session_uuid = ?1 AND agent_alias = ?2 AND killed_at IS NULL
             )",
            params![session_uuid, agent_alias],
            |row| row.get::<_, bool>(0),
        )
        .context("Failed to check live ACP session ownership")
    }

    /// Every durable ACP session key, used only to fail closed when a Chat key
    /// collides with the separate ACP namespace.
    pub fn list_session_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT session_uuid FROM acp_sessions")
            .context("Failed to prepare ACP session key query")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("Failed to query ACP session keys")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to read ACP session keys")
    }

    /// Load only durable ACP rows that are allowed to become live sessions.
    /// Killed rows keep their transcript for history/export but are terminal
    /// for runtime restore paths.
    pub fn load_session_for_restore(&self, session_uuid: &str) -> Result<AcpSessionRestore> {
        let conn = self.conn.lock();

        let row = conn.query_row(
            "SELECT id, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity, killed_at, trim_breadcrumb, principal_id
             FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        );

        let (
            session_id,
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count,
            created_at_s,
            last_activity_s,
            killed_at,
            trim_breadcrumb_raw,
            principal_id,
        ) = match row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(AcpSessionRestore::Missing),
            Err(e) => return Err(e).context("Failed to query ACP session for restore"),
        };

        if killed_at.is_some() {
            return Ok(AcpSessionRestore::Killed);
        }

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);
        let (messages, message_created_at) = Self::load_messages_with_times(&conn, session_id)?;
        let retained_context = Self::load_retained_context(&conn, session_id)?;
        let trim_breadcrumb = match trim_breadcrumb_raw {
            Some(v) => v != 0,
            None => {
                let inferred = Self::infer_legacy_trim_breadcrumb(&messages);
                Self::record_inferred_trim_breadcrumb(&conn, session_uuid, inferred)?;
                inferred
            }
        };

        Ok(AcpSessionRestore::Restorable(Box::new(AcpSessionData {
            session_uuid: session_uuid.to_string(),
            principal_id,
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
            message_created_at,
            trim_breadcrumb,
            retained_context,
        })))
    }

    /// List restorable sessions as lightweight summaries, ordered by most recent
    /// activity first. This is the picker-facing read: it avoids the full
    /// message-history hydration that `load_session` performs. Killed rows keep
    /// history/export data but are terminal and must not be offered for restore.
    /// Stale projected counts are rescored from the rows first, so the listing
    /// never reports a cache another writer left behind.
    pub fn list_sessions(&self) -> Result<Vec<AcpSessionSummary>> {
        let mut conn = self.conn.lock();
        Self::refresh_stale_projected_counts(&mut conn, ProjectedCountScope::Live)?;
        let mut stmt = conn
            .prepare(
                "SELECT s.session_uuid,
                        s.agent_alias,
                        s.workspace_dir,
                        s.token_count,
                        s.created_at,
                        s.last_activity,
                        s.projected_message_count AS message_count,
                        s.principal_id
                 FROM acp_sessions s
                 WHERE s.killed_at IS NULL
                 ORDER BY s.last_activity DESC",
            )
            .context("Failed to prepare ACP session list query")?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .context("Failed to query ACP sessions")?;

        let mut out = Vec::new();
        for row in rows {
            let (
                session_uuid,
                agent_alias,
                workspace_dir,
                token_count,
                created_s,
                activity_s,
                msg_count,
                principal_id,
            ) = row.context("Failed to read ACP session row")?;
            out.push(AcpSessionSummary {
                created_at: parse_ts(&created_s, "created_at", &session_uuid),
                last_activity: parse_ts(&activity_s, "last_activity", &session_uuid),
                session_uuid,
                principal_id,
                agent_alias,
                workspace_dir,
                token_count: token_count.max(0) as u64,
                message_count: msg_count.max(0) as usize,
            });
        }
        Ok(out)
    }

    /// List live ACP sessions owned by `agent_alias`, ordered by most recent
    /// activity. Killed rows are omitted from live discovery, but their retained
    /// transcripts remain readable through `load_session_for_agent` and they
    /// remain available to export through `list_sessions_by_agent`.
    pub fn list_live_sessions_by_agent(&self, agent_alias: &str) -> Result<Vec<AcpSessionSummary>> {
        let mut conn = self.conn.lock();
        Self::refresh_stale_projected_counts(
            &mut conn,
            ProjectedCountScope::LiveAgent(agent_alias),
        )?;
        let mut stmt = conn
            .prepare(
                "SELECT s.session_uuid,
                        s.agent_alias,
                        s.workspace_dir,
                        s.token_count,
                        s.created_at,
                        s.last_activity,
                        s.projected_message_count AS message_count,
                        s.principal_id
                 FROM acp_sessions s
                 WHERE s.agent_alias = ?1 AND s.killed_at IS NULL
                 ORDER BY s.last_activity DESC",
            )
            .context("Failed to prepare live ACP session query for agent")?;

        let rows = stmt
            .query_map(params![agent_alias], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .context("Failed to query live ACP sessions for agent")?;

        let mut out = Vec::new();
        for row in rows {
            let (
                session_uuid,
                owner_alias,
                workspace_dir,
                token_count,
                created_s,
                activity_s,
                msg_count,
                principal_id,
            ) = row.context("Failed to read live ACP session row")?;
            out.push(AcpSessionSummary {
                created_at: parse_ts(&created_s, "created_at", &session_uuid),
                last_activity: parse_ts(&activity_s, "last_activity", &session_uuid),
                session_uuid,
                principal_id,
                agent_alias: owner_alias,
                workspace_dir,
                token_count: token_count.max(0) as u64,
                message_count: msg_count.max(0) as usize,
            });
        }
        Ok(out)
    }

    fn load_messages(conn: &Connection, session_id: i64) -> Result<Vec<ConversationMessage>> {
        Ok(Self::load_messages_with_times(conn, session_id)?.0)
    }

    /// `load_messages` plus each message's row `created_at`, index-aligned.
    fn load_messages_with_times(
        conn: &Connection,
        session_id: i64,
    ) -> Result<(Vec<ConversationMessage>, Vec<Option<String>>)> {
        // Pull all message rows, excluding any `system` row. `insert_messages`
        // has never written one since the write-path filter that keeps the
        // Agent's system prompt out of authoritative replacements, but a
        // database created before that fix can still have one on disk;
        // filtering here is defense in depth so a restored session or a
        // `session/messages` read can't re-expose it even if a write path
        // regresses or an old row survives a partial migration.
        let mut msg_stmt = conn
            .prepare(
                "SELECT id, role, content, reasoning_content, created_at
                 FROM acp_messages WHERE session_id = ?1 AND role != 'system' ORDER BY id ASC",
            )
            .context("Failed to prepare message query")?;

        let msg_rows: Vec<MessageRow> = msg_stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to read message rows")?;

        // For each message row, pull its tool_calls (event_kind='in') and
        // tool_results (event_kind='out') in id order.
        let mut tc_stmt = conn
            .prepare(
                "SELECT tool_call_id, tool_name, event_kind, payload
                 FROM acp_tool_calls WHERE message_id = ?1 ORDER BY id ASC",
            )
            .context("Failed to prepare tool_call query")?;

        let mut out = Vec::with_capacity(msg_rows.len());
        let mut times = Vec::with_capacity(msg_rows.len());
        for (msg_id, role, content, reasoning_content, row_created_at) in msg_rows {
            // Split this message's tool_calls into ins and outs preserving order.
            let mut ins: Vec<ToolCall> = Vec::new();
            let mut outs: Vec<ToolResultMessage> = Vec::new();
            let rows = tc_stmt
                .query_map(params![msg_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to read tool_call rows")?;
            for (tool_call_id, tool_name, event_kind, payload) in rows {
                match event_kind.as_str() {
                    "in" => ins.push(ToolCall {
                        id: tool_call_id,
                        name: tool_name,
                        arguments: payload,
                        extra_content: None,
                    }),
                    "out" => outs.push(ToolResultMessage {
                        tool_call_id,
                        content: payload,
                        // Carry the producing tool name (looked up from the
                        // matching 'in' row on write) so a resumed session
                        // stays provenance-aware for media-marker
                        // canonicalization
                        tool_name,
                    }),
                    other => {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Read,
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "session_id": session_id,
                                "message_id": msg_id,
                                "event_kind": other,
                            })),
                            "unknown event_kind in acp_tool_calls"
                        );
                        return Err(anyhow::Error::msg(format!(
                            "unknown event_kind '{other}' in acp_tool_calls for message_id {msg_id}"
                        )));
                    }
                }
            }

            let role = if role == SYNTHETIC_INTERRUPTION_ROLE {
                "system".to_string()
            } else {
                role
            };
            // A row with no 'in' rows is a chat message, with any 'out' rows
            // following it as their own entries: the persisted counter
            // scores the parent row once for its text and each unmatched
            // result once, so the reload must present the same entries.
            if ins.is_empty() {
                out.push(ConversationMessage::Chat(ChatMessage { role, content }));
                if !outs.is_empty() {
                    out.push(ConversationMessage::ToolResults(outs));
                }
            } else {
                // Assistant turn that issued tool calls. The text may be empty.
                out.push(ConversationMessage::AssistantToolCalls {
                    text: if content.is_empty() {
                        None
                    } else {
                        Some(content)
                    },
                    tool_calls: ins,
                    reasoning_content,
                });
                if !outs.is_empty() {
                    out.push(ConversationMessage::ToolResults(outs));
                }
            }
            times.resize(out.len(), row_created_at);
        }

        Ok((out, times))
    }

    /// Refresh the persisted projected-entry counts whose watermark has gone
    /// stale, so the listing or getter about to read one never trusts a
    /// cache another writer left behind. Scoring is per session and
    /// non-fatal: each stale session is reloaded and rescored in its own
    /// short transaction from its rows through the shared counting rule. A
    /// session whose rows cannot be reloaded (an unknown `event_kind`) is
    /// logged at ERROR and left untouched: it still lists with its stored
    /// count (0 for a session never scored), still fails closed on
    /// `load_session`, and never blocks the store, the other sessions, or
    /// the open. A genuine SQLite failure on the scoring UPDATE (busy, disk
    /// full) propagates.
    fn refresh_stale_projected_counts(
        conn: &mut Connection,
        scope: ProjectedCountScope<'_>,
    ) -> Result<()> {
        let (filter_sql, filter_value) = scope.session_filter();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT s.id, s.session_uuid
                   FROM acp_sessions s
                  WHERE NOT (s.projected_count_through IS
                             (SELECT MAX(m.id) FROM acp_messages m
                               WHERE m.session_id = s.id))
                        {filter_sql}
                  ORDER BY s.id"
            ))
            .context("Failed to prepare stale projected-count query")?;
        let stale_sessions: Vec<(i64, String)> = match filter_value {
            Some(value) => stmt
                .query_map(params![value], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .context("Failed to read stale ACP sessions")?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to read stale ACP sessions")?,
            None => stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .context("Failed to read stale ACP sessions")?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to read stale ACP sessions")?,
        };
        drop(stmt);

        for (session_id, session_uuid) in stale_sessions {
            let tx = conn
                .transaction()
                .context("Failed to begin projected-count refresh transaction")?;
            let messages = match Self::load_messages(&tx, session_id) {
                Ok(messages) => messages,
                Err(error) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Read,)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "session_uuid": session_uuid,
                                "error": error.to_string(),
                            })),
                        "Failed to reload ACP session for projected-count refresh; leaving its persisted count untouched"
                    );
                    continue;
                }
            };
            let count = projected_entry_count(&messages) as i64;
            let watermark: Option<i64> = tx
                .query_row(
                    "SELECT MAX(id) FROM acp_messages WHERE session_id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .context("Failed to read ACP session watermark")?;
            tx.execute(
                "UPDATE acp_sessions
                    SET projected_message_count = ?1,
                        projected_count_through = ?2
                  WHERE id = ?3",
                params![count, watermark, session_id],
            )
            .context("Failed to refresh ACP session projected message count")?;
            tx.commit()
                .context("Failed to commit projected-count refresh")?;
        }
        Ok(())
    }

    fn load_retained_context(
        conn: &Connection,
        session_id: i64,
    ) -> Result<Option<Vec<ConversationMessage>>> {
        let payload: Option<String> = conn
            .query_row(
                "SELECT retained_context_json FROM acp_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read ACP retained context")?
            .flatten();
        let Some(payload) = payload else {
            return Ok(None);
        };
        let record = serde_json::from_str::<RetainedContextRecord>(&payload)
            .context("Failed to deserialize ACP retained context")?;
        Ok(Some(Self::provider_safe_history(&record.messages)))
    }

    /// Insert messages into the durable, user-visible transcript. This path
    /// deliberately excludes runtime system prompts; interruption markers use
    /// a provenance-known role and are added by recovery below.
    ///
    /// Returns the projected conversation-entry count of the inserted batch,
    /// folded against the tool-call rows already visible in this transaction:
    /// `append_messages` adds it to the persisted counter, and
    /// `replace_messages_inner` (whose prior rows the FK cascade already
    /// removed) sets the counter to it, which is the counting rule over the
    /// new transcript as a whole.
    fn insert_messages(
        tx: &Transaction<'_>,
        session_id: i64,
        messages: &[ConversationMessage],
        now: &str,
        prompt_at: Option<&str>,
    ) -> Result<i64> {
        // The first user row of a checkpointed turn is its prompt; it keeps
        // the time the turn began instead of the finalization time.
        let mut prompt_at = prompt_at;
        // Track the most recent assistant message_id so a following
        // ToolResults variant can attach its 'out' rows back to it.
        let mut last_assistant_msg_id: Option<i64> = None;
        // Persisted counter mirroring the runtime's projected conversation
        // length: one entry per chat message, non-empty assistant text plus
        // one per tool call, results folding into the call awaiting them.
        let mut projected_increment: i64 = 0;

        for msg in messages {
            match msg {
                ConversationMessage::Chat(chat) if chat.role == "system" => continue,
                ConversationMessage::Chat(chat) => {
                    let row_at = if chat.role == "user" {
                        prompt_at.take().unwrap_or(now)
                    } else {
                        now
                    };
                    tx.execute(
                        "INSERT INTO acp_messages
                           (session_id, role, content, reasoning_content, created_at)
                         VALUES (?1, ?2, ?3, NULL, ?4)",
                        params![session_id, chat.role, chat.content, row_at],
                    )
                    .context("Failed to insert chat message")?;
                    projected_increment += 1;
                    if chat.role == "assistant" {
                        last_assistant_msg_id = Some(tx.last_insert_rowid());
                    }
                }
                ConversationMessage::AssistantToolCalls {
                    text,
                    tool_calls,
                    reasoning_content,
                } => {
                    if text.as_deref().is_some_and(|text| !text.is_empty()) {
                        projected_increment += 1;
                    }
                    projected_increment += tool_calls.len() as i64;
                    // Zero-entry batch: nothing to persist, keeps the counter
                    // and the reloaded projection equal.
                    if tool_calls.is_empty()
                        && !text.as_deref().is_some_and(|text| !text.is_empty())
                    {
                        continue;
                    }
                    tx.execute(
                        "INSERT INTO acp_messages
                           (session_id, role, content, reasoning_content, created_at)
                         VALUES (?1, 'assistant', ?2, ?3, ?4)",
                        params![
                            session_id,
                            text.as_deref().unwrap_or(""),
                            reasoning_content,
                            now,
                        ],
                    )
                    .context("Failed to insert assistant tool-call message")?;
                    let msg_id = tx.last_insert_rowid();
                    last_assistant_msg_id = Some(msg_id);

                    for tc in tool_calls {
                        tx.execute(
                            "INSERT INTO acp_tool_calls
                               (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
                            params![
                                msg_id,
                                tc.id,
                                tc.name,
                                ToolEventKind::In.as_str(),
                                tc.arguments,
                                now,
                            ],
                        )
                        .context("Failed to insert tool_call 'in' row")?;
                    }
                }
                ConversationMessage::ToolResults(results) => {
                    let msg_id = match last_assistant_msg_id {
                        Some(id) => id,
                        None => {
                            ::zeroclaw_log::record!(
                                ERROR,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Write,
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "session_id": session_id,
                                })),
                                "ToolResults without preceding AssistantToolCalls"
                            );
                            return Err(anyhow::Error::msg(
                                "ToolResults appeared without a preceding AssistantToolCalls \
                                 message in this turn — cannot determine parent message_id",
                            ));
                        }
                    };
                    // This id's rows for the session, in write order; the
                    // fold below replays them through the shared clamp rule.
                    let mut history_stmt = tx
                        .prepare(
                            "SELECT tc.event_kind
                               FROM acp_tool_calls tc
                               JOIN acp_messages m ON m.id = tc.message_id
                              WHERE m.session_id = ?1 AND tc.tool_call_id = ?2
                              ORDER BY tc.id",
                        )
                        .context("Failed to prepare tool call history query")?;
                    for result in results {
                        let tool_name: String = tx
                            .query_row(
                                "SELECT tool_name FROM acp_tool_calls
                                 WHERE tool_call_id = ?1 AND event_kind = 'in'
                                 ORDER BY id DESC LIMIT 1",
                                params![result.tool_call_id],
                                |row| row.get(0),
                            )
                            .unwrap_or_else(|_| String::from("unknown"));
                        let events: Vec<ToolEventKind> = history_stmt
                            .query_map(params![session_id, result.tool_call_id], |row| {
                                row.get::<_, String>(0)
                            })
                            .context("Failed to look up tool call history")?
                            .collect::<Result<Vec<_>, _>>()
                            .context("Failed to read tool call history rows")?
                            .into_iter()
                            .map(|kind| ToolEventKind::parse(&kind))
                            .collect::<Option<Vec<_>>>()
                            .context("Unknown event_kind in tool call history")?;
                        // A result folds into its call (no entry) while an
                        // 'in' row for this tool_call_id is still unconsumed;
                        // beyond that it is an orphan entry. An orphan leaves
                        // the open-call balance untouched, so a later call
                        // reusing the id still opens fresh.
                        if open_calls_after(events) == 0 {
                            projected_increment += 1;
                        }
                        tx.execute(
                            "INSERT INTO acp_tool_calls
                               (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                msg_id,
                                result.tool_call_id,
                                tool_name,
                                ToolEventKind::Out.as_str(),
                                result.content,
                                EventOutcome::Unknown.as_str(),
                                now,
                            ],
                        )
                        .context("Failed to insert tool_call 'out' row")?;
                    }
                }
            }
        }

        Ok(projected_increment)
    }

    /// Append `messages` to the visible transcript and keep the persisted
    /// projected-entry count cache in step, in the caller's transaction.
    /// Every append path (`append_turn`, checkpoint finalization and
    /// recovery) goes through here. The batch's score is added only while
    /// the session's watermark still matches its rows; a cache a writer
    /// without the columns left behind is recomputed from the rows instead.
    fn append_messages(
        tx: &Transaction<'_>,
        session_uuid: &str,
        session_id: i64,
        messages: &[ConversationMessage],
        now: &str,
        prompt_at: Option<&str>,
    ) -> Result<()> {
        let messages = Self::bounded_transcript_messages(messages);
        // The persisted pair is a cache of the rows' projection, valid only
        // while the watermark matches the session's current MAX(id). Read
        // both before inserting: a mismatch means a writer this store did
        // not observe (a binary without the cache columns, or a raw insert
        // such as the recovery interruption marker) left rows past the
        // watermark, and the increment below would build on a stale base.
        let (watermark, max_id): (Option<i64>, Option<i64>) = tx
            .query_row(
                "SELECT s.projected_count_through,
                        (SELECT MAX(m.id) FROM acp_messages m
                          WHERE m.session_id = s.id)
                   FROM acp_sessions s
                  WHERE s.id = ?1",
                params![session_id],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .context("Failed to read ACP session projected count cache")?;
        let projected_increment = Self::insert_messages(tx, session_id, &messages, now, prompt_at)?;
        let watermark_now: Option<i64> = tx
            .query_row(
                "SELECT MAX(id) FROM acp_messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .context("Failed to read ACP session watermark")?;
        if watermark == max_id {
            // A valid cache: keep the increment and move the watermark to
            // the rows this transaction just wrote.
            tx.execute(
                "UPDATE acp_sessions
                    SET last_activity = ?1,
                        projected_message_count = projected_message_count + ?2,
                        projected_count_through = ?3
                  WHERE id = ?4",
                params![now, projected_increment, watermark_now, session_id],
            )
            .with_context(|| format!("Failed to update last_activity for {session_uuid}"))?;
            return Ok(());
        }
        // A stale cache: recompute the whole projection from the rows so the
        // increment never builds on the stale base. A session whose rows
        // cannot be reloaded keeps its stale pair untouched (the reload logs
        // it) and stays stale for a later read to heal once the rows are
        // repaired; the cache must not fail the append itself.
        match Self::load_messages(tx, session_id) {
            Ok(messages) => {
                let projected_count = projected_entry_count(&messages) as i64;
                tx.execute(
                    "UPDATE acp_sessions
                        SET last_activity = ?1,
                            projected_message_count = ?2,
                            projected_count_through = ?3
                      WHERE id = ?4",
                    params![now, projected_count, watermark_now, session_id],
                )
                .with_context(|| format!("Failed to update last_activity for {session_uuid}"))?;
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write,)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_uuid": session_uuid,
                            "error": error.to_string(),
                        })),
                    "Failed to reload ACP session for projected-count resync; leaving its persisted count untouched"
                );
                tx.execute(
                    "UPDATE acp_sessions SET last_activity = ?1 WHERE id = ?2",
                    params![now, session_id],
                )
                .with_context(|| format!("Failed to update last_activity for {session_uuid}"))?;
            }
        }
        Ok(())
    }

    fn append_checkpoint_visible_messages(
        tx: &Transaction<'_>,
        session_uuid: &str,
        session_id: i64,
        messages: &[ConversationMessage],
        now: &str,
        prompt_at: Option<&str>,
    ) -> Result<()> {
        // The checkpoint projection is the one owner-produced path allowed to
        // carry the synthetic interruption marker. Store it under an explicit
        // role so actual provider system prompts remain excluded everywhere
        // else and recovery can restore the marker without text heuristics.
        let messages = messages
            .iter()
            .map(|message| match message {
                ConversationMessage::Chat(chat) if chat.role == "system" => {
                    ConversationMessage::Chat(ChatMessage {
                        role: SYNTHETIC_INTERRUPTION_ROLE.to_string(),
                        content: chat.content.clone(),
                    })
                }
                other => other.clone(),
            })
            .collect::<Vec<_>>();
        Self::append_messages(tx, session_uuid, session_id, &messages, now, prompt_at)
    }

    fn session_id(conn: &Connection, session_uuid: &str) -> Result<i64> {
        conn.query_row(
            "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| row.get(0),
        )
        .with_context(|| format!("unknown session_uuid: {session_uuid}"))
    }

    pub fn contains_session(&self, session_uuid: &str) -> Result<bool> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM acp_sessions WHERE session_uuid = ?1)",
            params![session_uuid],
            |row| row.get(0),
        )
        .context("Failed to check ACP session existence")
    }

    /// Append all ConversationMessages from one completed turn in one transaction.
    pub fn append_turn(&self, session_uuid: &str, messages: &[ConversationMessage]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction()
            .context("Failed to begin append_turn transaction")?;
        Self::append_messages(&tx, session_uuid, session_id, messages, &now, None)?;

        tx.commit().context("Failed to commit append_turn")?;
        Ok(())
    }

    /// Replace a session's visible transcript while keeping message identity
    /// local to the transcript. Native retained provider context uses a
    /// separate projection and never calls this method for a trim.
    pub fn replace_messages(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        self.replace_messages_inner(session_uuid, messages, None)
    }

    pub fn replace_messages_and_breadcrumb(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
        breadcrumb_present: bool,
    ) -> Result<()> {
        self.replace_messages_inner(session_uuid, messages, Some(breadcrumb_present))
    }

    fn replace_messages_inner(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
        breadcrumb_present: Option<bool>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction()
            .context("Failed to begin replace_messages_and_breadcrumb transaction")?;
        tx.execute(
            "DELETE FROM acp_messages WHERE session_id = ?1",
            params![session_id],
        )
        .context("Failed to clear prior messages")?;
        // The cascade above removed the prior tool-call rows, so the fold
        // inside insert_messages starts from an empty open-call balance and
        // its return value is the projected entry count of the new
        // transcript as a whole: the counter is set to it, not incremented.
        let projected_count = Self::insert_messages(&tx, session_id, messages, &now, None)?;
        // The new transcript is authoritative, so the cache pair is set from
        // it: the count to its projection and the watermark to the rows just
        // written (NULL for an empty transcript, valid at 0).
        let watermark: Option<i64> = tx
            .query_row(
                "SELECT MAX(id) FROM acp_messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .context("Failed to read ACP session watermark")?;
        // Legacy replacement intentionally changes the source transcript;
        // invalidate its derived provider projection in the same transaction.
        tx.execute(
            "UPDATE acp_sessions SET last_activity = ?1, trim_breadcrumb = COALESCE(?2, trim_breadcrumb),
                retained_context_json = NULL, retained_context_frontier = NULL,
                projected_message_count = ?3, projected_count_through = ?4 WHERE id = ?5",
            params![
                now,
                breadcrumb_present.map(i64::from),
                projected_count,
                watermark,
                session_id
            ],
        )
        .context("Failed to update last_activity, trim_breadcrumb and projected_message_count")?;
        tx.commit()
            .context("Failed to commit replace_messages_and_breadcrumb")?;
        Ok(())
    }

    pub fn begin_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction()
            .context("Failed to begin ACP turn checkpoint transaction")?;
        tx.execute(
            "INSERT INTO acp_turn_checkpoints (session_id, turn_id, started_at)
             VALUES (?1, ?2, ?3)",
            params![session_id, turn_id, Utc::now().to_rfc3339()],
        )
        .context("Failed to begin ACP turn checkpoint")?;
        Self::append_checkpoint_events(&tx, session_id, messages)?;
        tx.commit()
            .context("Failed to commit ACP turn checkpoint")?;
        Ok(())
    }

    pub fn append_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<i64> {
        let mut conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction()
            .context("Failed to begin ACP turn checkpoint append")?;
        let active_turn: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read ACP turn checkpoint identity")?;
        anyhow::ensure!(
            active_turn.as_deref() == Some(turn_id),
            "ACP turn checkpoint identity mismatch"
        );
        let frontier = Self::append_checkpoint_events(&tx, session_id, messages)?;
        tx.commit()
            .context("Failed to commit ACP turn checkpoint append")?;
        Ok(frontier)
    }

    fn append_checkpoint_events(
        tx: &Transaction<'_>,
        session_id: i64,
        messages: &[ConversationMessage],
    ) -> Result<i64> {
        let mut frontier = tx
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM acp_turn_checkpoint_events WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .context("Failed to read ACP checkpoint frontier")?;
        for message in Self::bounded_transcript_messages(messages) {
            let payload = serde_json::to_string(&message)
                .context("Failed to serialize ACP turn checkpoint event")?;
            tx.execute(
                "INSERT INTO acp_turn_checkpoint_events (session_id, payload) VALUES (?1, ?2)",
                params![session_id, payload],
            )
            .context("Failed to append ACP turn checkpoint event")?;
            frontier = tx.last_insert_rowid();
        }
        Ok(frontier)
    }

    /// Atomically publish the provider-facing retained context and the exact
    /// checkpoint journal frontier observed by the serial RPC event consumer.
    /// The visible transcript remains untouched; its journal rows stay
    /// durable for recovery and `session/messages`.
    #[cfg(test)]
    fn persist_retained_context(
        &self,
        session_uuid: &str,
        turn_id: &str,
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
    ) -> Result<()> {
        // Tests consume journal writes synchronously. Native RPC records the
        // frontier at its ordered event boundary instead.
        let frontier = self.checkpoint_frontier(session_uuid, turn_id)?;
        self.persist_retained_context_at_frontier(
            session_uuid,
            turn_id,
            retained_messages,
            breadcrumb,
            frontier,
        )
    }

    /// Persist an owner-observed checkpoint frontier. The frontier is supplied
    /// by the serial event consumer after its append transaction; it is never
    /// inferred by content comparison or by a delayed global MAX in this path.
    pub fn persist_retained_context_at_frontier(
        &self,
        session_uuid: &str,
        turn_id: &str,
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
        frontier: i64,
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin retained context transaction")?;
        Self::ensure_active_checkpoint(&tx, session_id, turn_id)?;
        let retained = Self::bounded_transcript_messages(retained_messages);
        // This is the complete owner-selected projection, including partial
        // typed calls awaiting later results. Filtering belongs after recovery
        // composes the snapshot with uncovered journal events.
        let record = RetainedContextRecord {
            messages: Self::without_hidden_reasoning(&retained),
        };
        let payload =
            serde_json::to_string(&record).context("Failed to serialize retained ACP context")?;
        tx.execute(
            "UPDATE acp_sessions
                SET retained_context_json = ?1,
                    retained_context_frontier = ?2,
                    trim_breadcrumb = ?3,
                    last_activity = ?4
              WHERE id = ?5",
            params![
                payload,
                frontier,
                i64::from(breadcrumb),
                Utc::now().to_rfc3339(),
                session_id
            ],
        )
        .context("Failed to write retained ACP context")?;
        tx.commit()
            .context("Failed to commit retained ACP context")?;
        Ok(())
    }

    /// Persist a seed-time trim projection. No turn checkpoint exists during
    /// restore, so this path verifies only the session identity and commits the
    /// owner-produced snapshot before its notification is forwarded.
    pub fn persist_retained_context_seed(
        &self,
        session_uuid: &str,
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin retained seed context transaction")?;
        let retained = Self::bounded_transcript_messages(retained_messages);
        let record = RetainedContextRecord {
            messages: Self::without_hidden_reasoning(&Self::provider_safe_history(&retained)),
        };
        let payload =
            serde_json::to_string(&record).context("Failed to serialize retained seed context")?;
        tx.execute(
            "UPDATE acp_sessions
                SET retained_context_json = ?1,
                    retained_context_frontier = 0,
                    trim_breadcrumb = ?2,
                    last_activity = ?3
              WHERE id = ?4",
            params![
                payload,
                i64::from(breadcrumb),
                Utc::now().to_rfc3339(),
                session_id
            ],
        )
        .context("Failed to write retained seed context")?;
        tx.commit()
            .context("Failed to commit retained seed context")?;
        Ok(())
    }

    /// When the session's active checkpoint began, if the binary that began
    /// it recorded one.
    fn checkpoint_started_at(tx: &Transaction<'_>, session_id: i64) -> Result<Option<String>> {
        Ok(tx
            .query_row(
                "SELECT started_at FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .context("Failed to read ACP turn checkpoint start time")?
            .flatten())
    }

    pub fn checkpoint_frontier(&self, session_uuid: &str, turn_id: &str) -> Result<i64> {
        let conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let active: Option<String> = conn
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read ACP turn checkpoint identity")?;
        anyhow::ensure!(
            active.as_deref() == Some(turn_id),
            "ACP turn checkpoint identity mismatch"
        );
        conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM acp_turn_checkpoint_events WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .context("Failed to read ACP checkpoint frontier")
    }

    fn ensure_active_checkpoint(
        tx: &Transaction<'_>,
        session_id: i64,
        turn_id: &str,
    ) -> Result<()> {
        let active_turn: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read ACP turn checkpoint identity")?;
        anyhow::ensure!(
            active_turn.as_deref() == Some(turn_id),
            "ACP turn checkpoint identity mismatch"
        );
        Ok(())
    }

    fn without_hidden_reasoning(messages: &[ConversationMessage]) -> Vec<ConversationMessage> {
        messages
            .iter()
            .map(|message| match message {
                ConversationMessage::AssistantToolCalls {
                    text,
                    tool_calls,
                    reasoning_content: _,
                } => ConversationMessage::AssistantToolCalls {
                    text: text.clone(),
                    tool_calls: tool_calls.clone(),
                    reasoning_content: None,
                },
                other => other.clone(),
            })
            .collect()
    }

    /// Finalize a turn while atomically preserving visible journal fragments,
    /// the final provider projection, breadcrumb provenance and checkpoint
    /// deletion. Existing transcript rows are never replaced or re-identified.
    pub fn finalize_turn_checkpoint_with_context(
        &self,
        session_uuid: &str,
        turn_id: &str,
        terminal_messages: &[ConversationMessage],
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
    ) -> Result<()> {
        self.finalize_turn_checkpoint_with_context_for_owner(
            session_uuid,
            turn_id,
            terminal_messages,
            retained_messages,
            breadcrumb,
            None,
        )
    }

    /// Finalize only the durable row owned by the expected principal. The
    /// owner test and transcript, retained-context, and checkpoint changes
    /// occur in the same immediate transaction.
    pub fn finalize_turn_checkpoint_with_context_for_owner(
        &self,
        session_uuid: &str,
        turn_id: &str,
        terminal_messages: &[ConversationMessage],
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
        owner_principal_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin retained ACP turn finalization")?;
        let session_id: i64 = tx
            .query_row(
                "SELECT id FROM acp_sessions
                 WHERE session_uuid = ?1 AND (?2 IS NULL OR principal_id = ?2)",
                params![session_uuid, owner_principal_id],
                |row| row.get(0),
            )
            .context("ACP turn finalization owner mismatch or missing session")?;
        Self::ensure_active_checkpoint(&tx, session_id, turn_id)?;
        let payloads = {
            let mut stmt = tx
                .prepare(
                    "SELECT payload FROM acp_turn_checkpoint_events
                     WHERE session_id = ?1 ORDER BY id ASC",
                )
                .context("Failed to read ACP turn checkpoint events")?;
            stmt.query_map(params![session_id], |row| row.get::<_, String>(0))
                .context("Failed to query ACP turn checkpoint events")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("Failed to collect ACP turn checkpoint events")?
        };
        let fragments = payloads
            .into_iter()
            .map(|payload| {
                serde_json::from_str::<ConversationMessage>(&payload)
                    .context("Failed to deserialize ACP turn checkpoint event")
            })
            .collect::<Result<Vec<_>>>()?;
        // A successful or cooperative-cancel terminal delta is the canonical
        // current turn, including non-streamed output. The journal is its
        // crash fallback, not an additional transcript to append beside it.
        let visible = if terminal_messages.is_empty() {
            Self::fold_checkpoint_fragments(fragments)
        } else {
            Self::bounded_transcript_messages(terminal_messages)
        };
        let prompt_at = Self::checkpoint_started_at(&tx, session_id)?;
        Self::append_checkpoint_visible_messages(
            &tx,
            session_uuid,
            session_id,
            &visible,
            &now,
            prompt_at.as_deref(),
        )?;
        let retained = Self::bounded_transcript_messages(retained_messages);
        let record = RetainedContextRecord {
            messages: Self::without_hidden_reasoning(&Self::provider_safe_history(&retained)),
        };
        let payload = serde_json::to_string(&record)
            .context("Failed to serialize final retained ACP context")?;
        tx.execute(
            "UPDATE acp_sessions
                SET retained_context_json = ?1,
                    retained_context_frontier = 0,
                    trim_breadcrumb = ?2,
                    last_activity = ?3
              WHERE id = ?4",
            params![payload, i64::from(breadcrumb), now, session_id],
        )
        .context("Failed to write final retained ACP context")?;
        tx.execute(
            "DELETE FROM acp_turn_checkpoints WHERE session_id = ?1 AND turn_id = ?2",
            params![session_id, turn_id],
        )
        .context("Failed to delete finalized ACP turn checkpoint")?;
        tx.commit()
            .context("Failed to commit retained ACP turn finalization")?;
        Ok(())
    }

    pub fn discard_turn_checkpoint(&self, session_uuid: &str, turn_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let changed = conn
            .execute(
                "DELETE FROM acp_turn_checkpoints WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id, turn_id],
            )
            .context("Failed to discard ACP turn checkpoint")?;
        anyhow::ensure!(changed == 1, "ACP turn checkpoint identity mismatch");
        Ok(())
    }

    pub fn finalize_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction()
            .context("Failed to begin ACP turn finalization")?;
        let active_turn: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read ACP turn checkpoint identity")?;
        anyhow::ensure!(
            active_turn.as_deref() == Some(turn_id),
            "ACP turn checkpoint identity mismatch"
        );
        let messages = Self::bounded_transcript_messages(messages);
        let prompt_at = Self::checkpoint_started_at(&tx, session_id)?;
        Self::append_messages(
            &tx,
            session_uuid,
            session_id,
            &messages,
            &now,
            prompt_at.as_deref(),
        )?;
        tx.execute(
            "DELETE FROM acp_turn_checkpoints WHERE session_id = ?1 AND turn_id = ?2",
            params![session_id, turn_id],
        )
        .context("Failed to delete finalized ACP turn checkpoint")?;
        tx.commit()
            .context("Failed to commit ACP turn finalization")?;
        Ok(())
    }

    pub fn recover_turn_checkpoint(
        &self,
        session_uuid: &str,
        interruption_marker: &str,
    ) -> Result<bool> {
        self.recover_turn_checkpoint_for_owner(session_uuid, interruption_marker, None)
    }

    /// Recover an interrupted turn only while the durable row still belongs
    /// to `owner_principal_id`. The owner lookup and all checkpoint changes
    /// share one immediate transaction, so a same-ID replacement cannot be
    /// recovered under a stale principal authorization.
    pub fn recover_turn_checkpoint_for_owner(
        &self,
        session_uuid: &str,
        interruption_marker: &str,
        owner_principal_id: Option<&str>,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin ACP turn checkpoint recovery")?;
        let session_id = tx
            .query_row(
                "SELECT id FROM acp_sessions
                 WHERE session_uuid = ?1 AND (?2 IS NULL OR principal_id = ?2)",
                params![session_uuid, owner_principal_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to find ACP session for checkpoint recovery")?;
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        let killed_at: Option<String> = tx
            .query_row(
                "SELECT killed_at FROM acp_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .context("Failed to read ACP session killed marker")?;
        if killed_at.is_some() {
            return Ok(false);
        }
        let checkpoint_turn_id: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read ACP turn checkpoint")?;
        let Some(checkpoint_turn_id) = checkpoint_turn_id else {
            return Ok(false);
        };
        let mut statement = tx
            .prepare(
                "SELECT id, payload FROM acp_turn_checkpoint_events
                 WHERE session_id = ?1 ORDER BY id ASC",
            )
            .context("Failed to prepare ACP turn checkpoint event read")?;
        let payloads = statement
            .query_map(params![session_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .context("Failed to read ACP turn checkpoint events")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to collect ACP turn checkpoint events")?;
        drop(statement);
        let frontier: i64 = tx
            .query_row(
                "SELECT COALESCE(retained_context_frontier, 0) FROM acp_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .context("Failed to read retained context frontier")?;
        let fragments = payloads
            .into_iter()
            .map(|(_, payload)| {
                serde_json::from_str::<ConversationMessage>(&payload)
                    .context("Failed to deserialize ACP turn checkpoint event")
            })
            .collect::<Result<Vec<_>>>()?;
        let visible =
            Self::bounded_transcript_messages(&Self::fold_checkpoint_fragments(fragments));
        let prompt_at = Self::checkpoint_started_at(&tx, session_id)?;
        Self::append_messages(
            &tx,
            session_uuid,
            session_id,
            &visible,
            &now,
            prompt_at.as_deref(),
        )?;
        tx.execute(
            "INSERT INTO acp_messages
               (session_id, role, content, reasoning_content, created_at)
             VALUES (?1, ?2, ?3, NULL, ?4)",
            params![
                session_id,
                SYNTHETIC_INTERRUPTION_ROLE,
                interruption_marker,
                now
            ],
        )
        .context("Failed to persist synthetic interruption marker")?;
        if let Some(payload) = tx
            .query_row(
                "SELECT retained_context_json FROM acp_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .context("Failed to read retained context during recovery")?
            .flatten()
        {
            let mut record = serde_json::from_str::<RetainedContextRecord>(&payload)
                .context("Failed to deserialize retained context during recovery")?;
            let after_frontier = Self::payloads_after_frontier(&tx, session_id, frontier)?;
            let mut projected = record.messages.clone();
            projected.extend(after_frontier);
            // The serial frontier makes this concatenation disjoint; do not
            // use serialized-content overlap to repair a boundary because
            // identical user text and differently typed tool fragments are
            // both legitimate progress.
            record.messages =
                Self::provider_safe_history(&Self::fold_checkpoint_fragments(projected));
            tx.execute(
                "UPDATE acp_sessions SET retained_context_json = ?, retained_context_frontier = 0 WHERE id = ?",
                params![serde_json::to_string(&record)?, session_id],
            )
            .context("Failed to update retained context after recovery")?;
        }
        let changed = tx
            .execute(
                "DELETE FROM acp_turn_checkpoints WHERE session_id = ?1 AND turn_id = ?2",
                params![session_id, checkpoint_turn_id],
            )
            .context("Failed to delete recovered ACP turn checkpoint")?;
        anyhow::ensure!(changed == 1, "ACP turn checkpoint identity mismatch");
        tx.commit()
            .context("Failed to commit ACP turn checkpoint recovery")?;
        Ok(true)
    }

    fn payloads_after_frontier(
        tx: &Transaction<'_>,
        session_id: i64,
        frontier: i64,
    ) -> Result<Vec<ConversationMessage>> {
        let mut stmt = tx
            .prepare(
                "SELECT payload FROM acp_turn_checkpoint_events
                 WHERE session_id = ?1 AND id > ?2 ORDER BY id ASC",
            )
            .context("Failed to prepare retained context tail read")?;
        stmt.query_map(params![session_id, frontier], |row| row.get::<_, String>(0))
            .context("Failed to read retained context tail")?
            .map(|row| {
                row.context("Failed to read retained context tail row")
                    .and_then(|payload| {
                        serde_json::from_str::<ConversationMessage>(&payload)
                            .context("Failed to deserialize retained context tail")
                    })
            })
            .collect()
    }

    fn fold_checkpoint_fragments(fragments: Vec<ConversationMessage>) -> Vec<ConversationMessage> {
        let mut messages = Vec::new();
        for fragment in fragments {
            match fragment {
                ConversationMessage::Chat(chat) if chat.role == "assistant" => {
                    if let Some(ConversationMessage::Chat(previous)) = messages.last_mut()
                        && previous.role == "assistant"
                    {
                        previous.content.push_str(&chat.content);
                    } else {
                        messages.push(ConversationMessage::Chat(chat));
                    }
                }
                ConversationMessage::AssistantToolCalls {
                    mut text,
                    tool_calls,
                    reasoning_content,
                } => {
                    if text.is_none()
                        && let Some(ConversationMessage::Chat(previous)) = messages.last()
                        && previous.role == "assistant"
                    {
                        text = messages.pop().and_then(|message| match message {
                            ConversationMessage::Chat(chat) => {
                                (!chat.content.is_empty()).then_some(chat.content)
                            }
                            _ => None,
                        });
                    }
                    if let Some(ConversationMessage::AssistantToolCalls {
                        tool_calls: previous_calls,
                        ..
                    }) = messages.last_mut()
                    {
                        previous_calls.extend(tool_calls);
                    } else {
                        messages.push(ConversationMessage::AssistantToolCalls {
                            text,
                            tool_calls,
                            reasoning_content,
                        });
                    }
                }
                ConversationMessage::ToolResults(results) => {
                    if let Some(ConversationMessage::ToolResults(previous)) = messages.last_mut() {
                        previous.extend(results);
                    } else {
                        messages.push(ConversationMessage::ToolResults(results));
                    }
                }
                other => messages.push(other),
            }
        }
        messages
    }

    /// Apply the transcript's existing display/storage bound to native tool
    /// results before they enter a checkpoint or canonical ACP history.
    pub fn bounded_transcript_messages(
        messages: &[ConversationMessage],
    ) -> Vec<ConversationMessage> {
        messages
            .iter()
            .cloned()
            .map(|message| match message {
                ConversationMessage::ToolResults(mut results) => {
                    for result in &mut results {
                        result.content = Self::bounded_tool_output(&result.content);
                    }
                    ConversationMessage::ToolResults(results)
                }
                other => other,
            })
            .collect()
    }

    pub fn bounded_tool_output(output: &str) -> String {
        if output.len() <= MAX_PERSISTED_TOOL_OUTPUT_BYTES {
            return output.to_string();
        }
        let mut end = MAX_PERSISTED_TOOL_OUTPUT_BYTES;
        while end > 0 && !output.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…[truncated]", &output[..end])
    }

    /// Produce provider-safe ACP history while preserving client-visible text.
    /// Only an immediately adjacent tool-call/result pair is retained, and one
    /// result is kept for each unambiguous call id. Duplicate call or result IDs
    /// within a batch are rejected; recovery markers stay transcript-only.
    pub fn provider_safe_history(messages: &[ConversationMessage]) -> Vec<ConversationMessage> {
        let mut repaired = Vec::new();
        let mut index = 0;
        while index < messages.len() {
            match &messages[index] {
                ConversationMessage::Chat(chat)
                    if chat.role == "system" || chat.role == SYNTHETIC_INTERRUPTION_ROLE =>
                {
                    index += 1;
                }
                ConversationMessage::Chat(_) => {
                    repaired.push(messages[index].clone());
                    index += 1;
                }
                ConversationMessage::AssistantToolCalls {
                    text,
                    tool_calls,
                    reasoning_content,
                } => {
                    let adjacent_results =
                        messages.get(index + 1).and_then(|message| match message {
                            ConversationMessage::ToolResults(results) => Some(results),
                            _ => None,
                        });
                    let mut paired_calls = Vec::new();
                    let mut paired_results = Vec::new();
                    if let Some(results) = adjacent_results {
                        let mut call_id_counts = std::collections::HashMap::new();
                        for call in tool_calls {
                            *call_id_counts.entry(call.id.as_str()).or_insert(0usize) += 1;
                        }
                        for call in tool_calls {
                            // Reserving a result is insufficient: even with two
                            // results, duplicated call IDs cannot identify a pair.
                            if call_id_counts.get(call.id.as_str()) != Some(&1) {
                                continue;
                            }
                            let mut matching_results = results
                                .iter()
                                .filter(|result| result.tool_call_id == call.id);
                            if let Some(result) = matching_results.next()
                                && matching_results.next().is_none()
                            {
                                paired_calls.push(call.clone());
                                paired_results.push(result.clone());
                            }
                        }
                    }

                    if paired_calls.is_empty() {
                        if let Some(text) = text.as_ref().filter(|text| !text.is_empty()) {
                            repaired.push(ConversationMessage::Chat(ChatMessage::assistant(text)));
                        }
                    } else {
                        repaired.push(ConversationMessage::AssistantToolCalls {
                            text: text.clone(),
                            tool_calls: paired_calls,
                            reasoning_content: reasoning_content.clone(),
                        });
                        repaired.push(ConversationMessage::ToolResults(paired_results));
                    }
                    index += if adjacent_results.is_some() { 2 } else { 1 };
                }
                ConversationMessage::ToolResults(_) => {
                    index += 1;
                }
            }
        }
        repaired
    }

    pub fn set_token_count(&self, session_uuid: &str, token_count: u64) -> Result<()> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE acp_sessions SET token_count = ?1 WHERE session_uuid = ?2",
                params![token_count as i64, session_uuid],
            )
            .context("Failed to set token_count")?;
        if rows == 0 {
            return Err(anyhow::Error::msg(format!(
                "set_token_count: no session with uuid {session_uuid}"
            )));
        }
        Ok(())
    }

    /// Clear the durable token snapshot back to the schema's unknown
    /// representation (0). Used when an accepted turn attempt serves a route
    /// without token usage: the prior snapshot must not survive, mirroring
    /// the client-side meter which clears on accepted usage-less events.
    pub fn clear_token_count(&self, session_uuid: &str) -> Result<()> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE acp_sessions SET token_count = 0 WHERE session_uuid = ?1",
                params![session_uuid],
            )
            .context("Failed to clear token_count")?;
        if rows == 0 {
            return Err(anyhow::Error::msg(format!(
                "clear_token_count: no session with uuid {session_uuid}"
            )));
        }
        Ok(())
    }

    /// Apply a `TurnEvent::Usage` to the durable token snapshot. Accepted
    /// events with usage overwrite it; accepted usage-less events clear it
    /// back to unknown (0) so a resumed session never replays a stale
    /// route's count; rejected billing telemetry never touches the store.
    /// Both durable consumers (ACP server, RPC dispatch) route through here
    /// so the accepted-gate cannot drift between paths.
    pub fn persist_usage_snapshot(
        &self,
        session_uuid: &str,
        input_tokens: Option<u64>,
        accepted: bool,
    ) -> Result<()> {
        if !accepted {
            return Ok(());
        }
        match input_tokens {
            Some(v) => self.set_token_count(session_uuid, v),
            None => self.clear_token_count(session_uuid),
        }
    }

    /// Record whether this session's persisted transcript currently starts
    /// with the synthetic trim breadcrumb, as one canonical fact alongside
    /// the transcript. Silently no-ops for an unknown session (matching
    /// `append_turn`'s tolerance for a session removed mid-turn) rather than
    /// erroring like `set_token_count`, since this is best-effort bookkeeping
    /// that must never fail a turn.
    pub fn set_trim_breadcrumb(&self, session_uuid: &str, present: bool) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE acp_sessions SET trim_breadcrumb = ?1 WHERE session_uuid = ?2",
            params![i64::from(present), session_uuid],
        )
        .context("Failed to set trim_breadcrumb")?;
        Ok(())
    }

    /// Persist the session's latest TodoWrite plan as a JSON array of
    /// `PlanEntry` (whole-list replace). An empty slice stores an empty
    /// array (a cleared plan), distinct from SQL NULL (never had one).
    pub fn set_plan(&self, session_uuid: &str, entries: &[PlanEntry]) -> Result<()> {
        let plan_json =
            serde_json::to_string(entries).context("Failed to serialize plan entries")?;
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE acp_sessions SET plan_json = ?1 WHERE session_uuid = ?2",
                params![plan_json, session_uuid],
            )
            .context("Failed to set plan_json")?;
        if rows == 0 {
            return Err(anyhow::Error::msg(format!(
                "set_plan: no session with uuid {session_uuid}"
            )));
        }
        Ok(())
    }

    /// Load the session's stored plan. Returns an empty vec when the
    /// session has no plan (NULL or absent). Malformed JSON is treated
    /// as an empty plan rather than a hard error, so a corrupt plan
    /// column never blocks session restore.
    pub fn get_plan(&self, session_uuid: &str) -> Result<Vec<PlanEntry>> {
        let conn = self.conn.lock();
        let plan_json: Option<String> = conn
            .query_row(
                "SELECT plan_json FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to query plan_json")?
            .flatten();
        Ok(plan_json
            .and_then(|s| serde_json::from_str::<Vec<PlanEntry>>(&s).ok())
            .unwrap_or_default())
    }

    /// Record a session-lifecycle event. Caller passes typed enums; the SQLite
    /// layer is the only place strings appear. Same `Action` / `EventOutcome`
    /// values are used at the matching `zeroclaw_log::record!` call site.
    pub fn append_event(
        &self,
        session_uuid: &str,
        action: Action,
        outcome: EventOutcome,
        payload: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        let session_id: i64 = conn
            .query_row(
                "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| row.get(0),
            )
            .with_context(|| format!("unknown session_uuid: {session_uuid}"))?;
        conn.execute(
            "INSERT INTO acp_session_events
               (session_id, action, outcome, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, action.as_str(), outcome.as_str(), payload, now],
        )
        .context("Failed to insert session event")?;
        Ok(())
    }

    /// Delete a session and all its child rows (messages, tool calls, events
    /// cascade via FK). Returns `true` if the session existed.
    pub fn delete_session(&self, session_uuid: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "DELETE FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
            )
            .context("Failed to delete ACP session")?;
        Ok(rows > 0)
    }

    // ── per-agent cascade (agent deletion,───────────────────────────

    /// Count *live* ACP sessions for `agent_alias` — rows not yet killed
    /// (`killed_at IS NULL`). A non-zero count is a HARD blocker for deleting the
    /// agent: the operator must end the sessions first.
    pub fn count_live_sessions_by_agent(&self, agent_alias: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM acp_sessions WHERE agent_alias = ?1 AND killed_at IS NULL",
                params![agent_alias],
                |row| row.get(0),
            )
            .context("Failed to count live ACP sessions for agent")?;
        Ok(n.max(0) as usize)
    }

    /// Summaries of every ACP session (live or killed) attributed to
    /// `agent_alias`, for the export-then-delete archive.
    pub fn list_sessions_by_agent(&self, agent_alias: &str) -> Result<Vec<AcpSessionSummary>> {
        let mut conn = self.conn.lock();
        Self::refresh_stale_projected_counts(&mut conn, ProjectedCountScope::Agent(agent_alias))?;
        let mut stmt = conn
            .prepare(
                "SELECT s.session_uuid,
                        s.agent_alias,
                        s.workspace_dir,
                        s.token_count,
                        s.created_at,
                        s.last_activity,
                        s.projected_message_count AS message_count,
                        s.principal_id
                 FROM acp_sessions s
                 WHERE s.agent_alias = ?1
                 ORDER BY s.last_activity DESC",
            )
            .context("Failed to prepare ACP per-agent session query")?;

        let rows = stmt
            .query_map(params![agent_alias], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .context("Failed to query ACP sessions for agent")?;

        let mut out = Vec::new();
        for row in rows {
            let (
                session_uuid,
                agent_alias,
                workspace_dir,
                token_count,
                created_s,
                activity_s,
                msg_count,
                principal_id,
            ) = row.context("Failed to read ACP session row")?;
            out.push(AcpSessionSummary {
                created_at: parse_ts(&created_s, "created_at", &session_uuid),
                last_activity: parse_ts(&activity_s, "last_activity", &session_uuid),
                session_uuid,
                principal_id,
                agent_alias,
                workspace_dir,
                token_count: token_count.max(0) as u64,
                message_count: msg_count.max(0) as usize,
            });
        }
        Ok(out)
    }

    /// Persisted projected conversation-entry count for the session, the
    /// same number the session picker lists and `turn_end` reports. Reading
    /// it avoids hydrating the full message history that `load_session`
    /// performs just to re-derive the count; a cache another writer left
    /// stale is rescored from the session's rows here first. `None` when the
    /// session UUID is unknown.
    pub fn projected_message_count(&self, session_uuid: &str) -> Result<Option<usize>> {
        let mut conn = self.conn.lock();
        Self::refresh_stale_projected_counts(
            &mut conn,
            ProjectedCountScope::Session(session_uuid),
        )?;
        let count: Option<i64> = conn
            .query_row(
                "SELECT projected_message_count FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to query projected_message_count")?;
        Ok(count.map(|n| n.max(0) as usize))
    }

    /// Delete every ACP session (live or killed) for `agent_alias`, returning the
    /// row count. Child tables (`acp_messages`/`acp_tool_calls`/`acp_session_events`)
    /// cascade via their `ON DELETE CASCADE` FKs (`foreign_keys = ON`).
    pub fn delete_sessions_by_agent(&self, agent_alias: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "DELETE FROM acp_sessions WHERE agent_alias = ?1",
                params![agent_alias],
            )
            .context("Failed to delete ACP sessions for agent")?;
        Ok(rows)
    }

    /// Re-point every ACP session (live or killed) from `from` to `to`,
    /// returning the row count. The agent-rename cascadekeeps the
    /// session and its transcript; only the owning alias moves. Unlike delete,
    /// a live session (`killed_at IS NULL`) is no obstacle to rename.
    pub fn rename_sessions_by_agent(&self, from: &str, to: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE acp_sessions SET agent_alias = ?2 WHERE agent_alias = ?1",
                params![from, to],
            )
            .context("Failed to rename ACP session owner")?;
        Ok(rows)
    }

    /// Atomically persist that an admin intentionally killed this ACP session.
    /// The transcript and any checkpoint stay durable, but runtime rehydration
    /// must not revive it.
    pub fn mark_session_killed_atomic(
        &self,
        session_uuid: &str,
    ) -> Result<AcpSessionKillTransition> {
        self.mark_session_killed_atomic_for_owner(session_uuid, None)
    }

    /// Tombstone only the durable row still owned by this principal. The
    /// update predicate and transition inspection share one transaction.
    pub fn mark_session_killed_atomic_for_owner(
        &self,
        session_uuid: &str,
        owner_principal_id: Option<&str>,
    ) -> Result<AcpSessionKillTransition> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin ACP session kill transition")?;
        let rows = tx
            .execute(
                "UPDATE acp_sessions
                    SET killed_at = COALESCE(killed_at, ?1),
                        last_activity = ?1
                  WHERE session_uuid = ?2 AND killed_at IS NULL
                    AND (?3 IS NULL OR principal_id = ?3)",
                params![now, session_uuid, owner_principal_id],
            )
            .context("Failed to mark ACP session killed")?;
        let transition = if rows == 1 {
            AcpSessionKillTransition::Marked
        } else {
            let killed_at: Option<Option<String>> = tx
                .query_row(
                    "SELECT killed_at FROM acp_sessions
                     WHERE session_uuid = ?1 AND (?2 IS NULL OR principal_id = ?2)",
                    params![session_uuid, owner_principal_id],
                    |row| row.get(0),
                )
                .optional()
                .context("Failed to inspect ACP session kill transition")?;
            match killed_at {
                Some(Some(_)) => AcpSessionKillTransition::AlreadyKilled,
                Some(None) => {
                    anyhow::bail!("ACP session kill transition made no durable change")
                }
                None => AcpSessionKillTransition::Missing,
            }
        };
        tx.commit()
            .context("Failed to commit ACP session kill transition")?;
        Ok(transition)
    }

    /// Persist that an admin intentionally killed this ACP session. The
    /// transcript stays durable, but runtime rehydration must not revive it.
    /// This compatibility wrapper retains the original boolean contract for
    /// existing callers; use `mark_session_killed_atomic` when the distinction
    /// between marked, already-killed, and missing matters.
    pub fn mark_session_killed(&self, session_uuid: &str) -> Result<bool> {
        Ok(matches!(
            self.mark_session_killed_atomic(session_uuid)?,
            AcpSessionKillTransition::Marked | AcpSessionKillTransition::AlreadyKilled
        ))
    }

    /// Return whether this durable ACP session has been intentionally killed.
    /// Missing rows are not killed; callers can then use normal load handling
    /// to distinguish SESSION_NOT_FOUND from a terminal killed session.
    pub fn is_session_killed(&self, session_uuid: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT CASE WHEN killed_at IS NULL THEN 0 ELSE 1 END
             FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| row.get::<_, i64>(0),
        );
        match row {
            Ok(killed) => Ok(killed != 0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e).context("Failed to query ACP session killed marker"),
        }
    }

    /// Update `last_activity` without appending messages.
    pub fn touch_session(&self, session_uuid: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE acp_sessions SET last_activity = ?1 WHERE session_uuid = ?2",
            params![now, session_uuid],
        )
        .context("Failed to touch ACP session")?;
        Ok(())
    }
}

struct ProjectedGroup {
    message_id: i64,
    role: String,
    content: String,
    reasoning_content: Option<String>,
    has_tool_events: bool,
    input_count: usize,
    unmatched_output_count: usize,
}

impl ProjectedGroup {
    fn len(&self) -> usize {
        if !self.has_tool_events {
            1
        } else {
            usize::from(!self.content.is_empty()) + self.input_count + self.unmatched_output_count
        }
    }

    fn ensure_well_formed(&self, conn: &Connection) -> Result<()> {
        let malformed: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM acp_tool_calls
                 WHERE message_id = ?1 AND event_kind NOT IN ('in', 'out')
             )",
            params![self.message_id],
            |row| row.get(0),
        )?;
        if malformed {
            return Err(anyhow::Error::msg(format!(
                "unknown event_kind in acp_tool_calls for message_id {}",
                self.message_id
            )));
        }
        Ok(())
    }

    fn entries_to_messages(
        &self,
        conn: &Connection,
        range: std::ops::Range<usize>,
        session_id: i64,
        max_message_id: i64,
        message_id: i64,
    ) -> Result<Vec<ConversationMessage>> {
        let mut messages = Vec::new();
        if !self.has_tool_events {
            if range.start == 0 && range.end > 0 {
                messages.push(ConversationMessage::Chat(ChatMessage {
                    role: if self.role == SYNTHETIC_INTERRUPTION_ROLE {
                        "system".to_string()
                    } else {
                        self.role.clone()
                    },
                    content: self.content.clone(),
                }));
            }
            return Ok(messages);
        }
        if range.start < range.end {
            self.ensure_well_formed(conn)?;
        }
        let mut selected_ids = std::collections::HashSet::new();
        let narration = usize::from(!self.content.is_empty());
        if range.start < narration && !self.content.is_empty() {
            messages.push(ConversationMessage::Chat(ChatMessage {
                role: "assistant".to_string(),
                content: self.content.clone(),
            }));
        }

        let input_start = range.start.saturating_sub(narration);
        let input_end = range.end.saturating_sub(narration).min(self.input_count);
        if input_start < input_end {
            let selected = input_end - input_start;
            let mut stmt = conn.prepare(
                "WITH selected_inputs AS (
                     SELECT id, tool_call_id
                     FROM acp_tool_calls
                     WHERE message_id = ?1 AND event_kind = 'in'
                     ORDER BY id LIMIT ?2 OFFSET ?3
                 ), selected_ids AS (
                     SELECT DISTINCT tool_call_id FROM selected_inputs
                 ), ranked_inputs AS (
                     SELECT input_rows.id, input_rows.tool_call_id,
                            ROW_NUMBER() OVER (
                                PARTITION BY input_rows.tool_call_id ORDER BY input_rows.id
                            ) - 1 AS input_ordinal
                     FROM acp_tool_calls input_rows
                     JOIN selected_ids USING (tool_call_id)
                     WHERE input_rows.message_id = ?1 AND input_rows.event_kind = 'in'
                 ), ranked_outputs AS (
                     SELECT output_rows.id, output_rows.tool_call_id,
                            ROW_NUMBER() OVER (
                                PARTITION BY output_rows.tool_call_id ORDER BY output_rows.id
                            ) - 1 AS output_ordinal
                     FROM acp_tool_calls output_rows
                     JOIN selected_ids USING (tool_call_id)
                     WHERE output_rows.message_id = ?1 AND output_rows.event_kind = 'out'
                 )
                 SELECT input_rows.tool_call_id, input_rows.tool_name,
                        input_rows.payload, output_rows.payload,
                        output_rows.tool_name
                 FROM selected_inputs
                 JOIN ranked_inputs ON ranked_inputs.id = selected_inputs.id
                 JOIN acp_tool_calls input_rows ON input_rows.id = selected_inputs.id
                 LEFT JOIN ranked_outputs
                   ON ranked_outputs.tool_call_id = selected_inputs.tool_call_id
                  AND ranked_outputs.output_ordinal = ranked_inputs.input_ordinal
                 LEFT JOIN acp_tool_calls output_rows ON output_rows.id = ranked_outputs.id
                 ORDER BY input_rows.id",
            )?;
            let rows = stmt.query_map(
                params![self.message_id, selected as i64, input_start as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?;
            for row in rows {
                let (tool_call_id, tool_name, arguments, output, output_name) = row?;
                selected_ids.insert(tool_call_id.clone());
                messages.push(ConversationMessage::AssistantToolCalls {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: tool_call_id.clone(),
                        name: tool_name,
                        arguments,
                        extra_content: None,
                    }],
                    reasoning_content: self.reasoning_content.clone(),
                });
                if let Some(content) = output {
                    messages.push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                        tool_call_id,
                        content,
                        tool_name: output_name.unwrap_or_default(),
                    }]));
                }
            }
        }

        let output_start = range.start.saturating_sub(narration + self.input_count);
        let output_end = range
            .end
            .saturating_sub(narration + self.input_count)
            .min(self.unmatched_output_count);
        if output_start < output_end {
            let selected = output_end - output_start;
            let mut stmt = conn.prepare(
                "WITH outputs AS (
                     SELECT id, tool_call_id,
                            ROW_NUMBER() OVER (
                                PARTITION BY tool_call_id ORDER BY id
                            ) - 1 AS output_ordinal
                     FROM acp_tool_calls
                     WHERE message_id = ?1 AND event_kind = 'out'
                 ), input_counts AS (
                     SELECT tool_call_id, COUNT(*) AS input_count
                     FROM acp_tool_calls
                     WHERE message_id = ?1 AND event_kind = 'in'
                     GROUP BY tool_call_id
                 ), selected_outputs AS (
                     SELECT outputs.id
                     FROM outputs LEFT JOIN input_counts USING (tool_call_id)
                     WHERE outputs.output_ordinal >= COALESCE(input_counts.input_count, 0)
                     ORDER BY outputs.id LIMIT ?2 OFFSET ?3
                 )
                 SELECT output_rows.tool_call_id, output_rows.tool_name,
                        output_rows.payload
                 FROM selected_outputs
                 JOIN acp_tool_calls output_rows ON output_rows.id = selected_outputs.id
                 ORDER BY output_rows.id",
            )?;
            let rows = stmt.query_map(
                params![self.message_id, selected as i64, output_start as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            for row in rows {
                let (tool_call_id, tool_name, content) = row?;
                selected_ids.insert(tool_call_id.clone());
                messages.push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                    tool_call_id,
                    content,
                    tool_name,
                }]));
            }
        }
        let selected_ids = selected_ids.into_iter().collect::<Vec<_>>();
        for ids in selected_ids.chunks(900) {
            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT tc.tool_call_id
                 FROM acp_tool_calls tc
                 JOIN acp_messages m ON m.id = tc.message_id
                 WHERE m.session_id = ? AND m.id <= ? AND m.id != ?
                   AND tc.tool_call_id IN ({placeholders})
                 LIMIT 1"
            );
            let mut values = vec![
                rusqlite::types::Value::Integer(session_id),
                rusqlite::types::Value::Integer(max_message_id),
                rusqlite::types::Value::Integer(message_id),
            ];
            values.extend(ids.iter().cloned().map(rusqlite::types::Value::Text));
            let reused = conn
                .query_row(&sql, rusqlite::params_from_iter(values), |row| {
                    row.get::<_, String>(0)
                })
                .optional()?;
            if let Some(tool_call_id) = reused {
                return Err(anyhow::Error::msg(format!(
                    "cross-group ACP tool_call_id reuse: {tool_call_id}"
                )));
            }
        }
        Ok(messages)
    }
}

fn load_projected_group(
    conn: &Connection,
    message_id: i64,
    role: String,
    content: String,
    reasoning_content: Option<String>,
) -> Result<ProjectedGroup> {
    let has_tool_events: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM acp_tool_calls WHERE message_id = ?1
         )",
        params![message_id],
        |row| row.get(0),
    )?;
    if !has_tool_events {
        return Ok(ProjectedGroup {
            message_id,
            role,
            content,
            reasoning_content,
            has_tool_events,
            input_count: 0,
            unmatched_output_count: 0,
        });
    }
    let input_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM acp_tool_calls
         WHERE message_id = ?1 AND event_kind = 'in'",
        params![message_id],
        |row| row.get(0),
    )?;
    let unmatched_output_count: i64 = conn.query_row(
        "WITH outputs AS (
             SELECT tool_call_id, COUNT(*) AS output_count
             FROM acp_tool_calls
             WHERE message_id = ?1 AND event_kind = 'out'
             GROUP BY tool_call_id
         ), inputs AS (
             SELECT tool_call_id, COUNT(*) AS input_count
             FROM acp_tool_calls
             WHERE message_id = ?1 AND event_kind = 'in'
             GROUP BY tool_call_id
         )
         SELECT COALESCE(SUM(
             CASE WHEN outputs.output_count > COALESCE(inputs.input_count, 0)
                  THEN outputs.output_count - COALESCE(inputs.input_count, 0)
                  ELSE 0 END
         ), 0)
         FROM outputs LEFT JOIN inputs USING (tool_call_id)",
        params![message_id],
        |row| row.get(0),
    )?;
    Ok(ProjectedGroup {
        message_id,
        role,
        content,
        reasoning_content,
        has_tool_events,
        input_count: input_count.max(0) as usize,
        unmatched_output_count: unmatched_output_count.max(0) as usize,
    })
}

fn encode_cursor(cursor: AcpSessionCursor) -> Result<String> {
    let bytes = serde_json::to_vec(&cursor)?;
    let mut encoded = String::from("acp1.");
    for byte in bytes {
        use std::fmt::Write;
        write!(encoded, "{byte:02x}")?;
    }
    Ok(encoded)
}

fn decode_cursor(encoded: &str) -> Result<AcpSessionCursor> {
    let hex = encoded
        .strip_prefix("acp1.")
        .ok_or_else(|| anyhow::Error::msg("invalid ACP session cursor"))?;
    if !hex.is_ascii() || hex.len() % 2 != 0 || hex.len() > 2048 {
        return Err(anyhow::Error::msg("invalid ACP session cursor"));
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow::Error::msg("invalid ACP session cursor"))?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::Error::msg("invalid ACP session cursor"))
}

fn parse_ts(s: &str, field: &'static str, session_uuid: &str) -> DateTime<Utc> {
    s.parse::<DateTime<Utc>>().unwrap_or_else(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "session_uuid": session_uuid,
                    "field": field,
                    "error": e.to_string(),
                })
            ),
            "Failed to parse session timestamp"
        );
        Utc::now()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_api::model_provider::{
        ChatMessage, ToolCall, ToolResultMessage, projected_entry_count,
    };

    fn open_store() -> (TempDir, AcpSessionStore) {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        (tmp, store)
    }

    #[test]
    fn retained_context_two_trims_recover_once_without_rewriting_originals() {
        let (tmp, store) = open_store();
        let sid = "retained-recovery";
        store.create_session(sid, "agent", "/tmp", None).unwrap();
        let old = ConversationMessage::Chat(ChatMessage::user("archived request"));
        store.append_turn(sid, &[old]).unwrap();
        let original_id: i64 = store
            .conn
            .lock()
            .query_row("SELECT id FROM acp_messages", [], |row| row.get(0))
            .unwrap();
        store
            .begin_turn_checkpoint(
                sid,
                "turn",
                &[ConversationMessage::Chat(ChatMessage::user(
                    "active request",
                ))],
            )
            .unwrap();
        store
            .append_turn_checkpoint(
                sid,
                "turn",
                &[ConversationMessage::Chat(ChatMessage::assistant(
                    "earlier progress",
                ))],
            )
            .unwrap();
        store
            .persist_retained_context(
                sid,
                "turn",
                &[
                    ConversationMessage::Chat(ChatMessage::user("active request")),
                    ConversationMessage::Chat(ChatMessage::assistant("earlier progress")),
                ],
                false,
            )
            .unwrap();
        let call = ConversationMessage::AssistantToolCalls {
            text: Some("checking".into()),
            tool_calls: vec![ToolCall {
                id: "retained-call".into(),
                name: "read".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        };
        store
            .append_turn_checkpoint(sid, "turn", std::slice::from_ref(&call))
            .unwrap();
        // A later real trim deliberately drops earlier active progress. The
        // retained call is still incomplete at the snapshot boundary.
        store
            .persist_retained_context(
                sid,
                "turn",
                &[
                    ConversationMessage::Chat(ChatMessage::user("retry request")),
                    call,
                ],
                false,
            )
            .unwrap();
        store
            .append_turn_checkpoint(
                sid,
                "turn",
                &[
                    ConversationMessage::ToolResults(vec![ToolResultMessage {
                        tool_call_id: "retained-call".into(),
                        tool_name: "read".into(),
                        content: "result".into(),
                    }]),
                    ConversationMessage::Chat(ChatMessage::assistant("after snapshot")),
                ],
            )
            .unwrap();
        drop(store);
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert!(store.recover_turn_checkpoint(sid, "interrupted").unwrap());
        let recovered = store.load_session(sid).unwrap().unwrap();
        let model = recovered.retained_context.unwrap();
        assert_eq!(model.len(), 4);
        assert!(matches!(
            &model[1],
            ConversationMessage::AssistantToolCalls { .. }
        ));
        assert!(matches!(&model[2], ConversationMessage::ToolResults(_)));
        let model_json = serde_json::to_string(&model).unwrap();
        assert!(!model_json.contains("earlier progress"));
        assert!(!model_json.contains("archived request"));
        let transcript = serde_json::to_string(&recovered.messages).unwrap();
        assert!(transcript.contains("earlier progress"));
        assert!(transcript.contains("archived request"));
        assert_eq!(transcript.matches("after snapshot").count(), 1);
        let preserved_id: i64 = store
            .conn
            .lock()
            .query_row("SELECT MIN(id) FROM acp_messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(original_id, preserved_id);
        drop(store);
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert!(!store.recover_turn_checkpoint(sid, "interrupted").unwrap());
        assert_eq!(
            serde_json::to_string(&store.load_session(sid).unwrap().unwrap().messages).unwrap(),
            transcript
        );
    }

    #[test]
    fn empty_retention_is_authoritative_and_stale_turn_cannot_replace_it() {
        let (_tmp, store) = open_store();
        let sid = "empty-retention";
        store.create_session(sid, "agent", "/tmp", None).unwrap();
        store
            .append_turn(
                sid,
                &[ConversationMessage::Chat(ChatMessage::user("archived"))],
            )
            .unwrap();
        store.begin_turn_checkpoint(sid, "current", &[]).unwrap();
        store
            .persist_retained_context(sid, "current", &[], false)
            .unwrap();
        assert!(
            store
                .persist_retained_context(
                    sid,
                    "stale",
                    &[ConversationMessage::Chat(ChatMessage::user("wrong")),],
                    false
                )
                .is_err()
        );
        let data = store.load_session(sid).unwrap().unwrap();
        assert_eq!(data.messages.len(), 1);
        assert!(data.retained_context.unwrap().is_empty());
        assert!(store.recover_turn_checkpoint(sid, "interrupted").unwrap());
        assert!(
            store
                .load_session(sid)
                .unwrap()
                .unwrap()
                .retained_context
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn legacy_replacement_invalidates_retained_context() {
        let (_tmp, store) = open_store();
        let sid = "legacy-replacement";
        store.create_session(sid, "agent", "/tmp", None).unwrap();
        store.persist_retained_context_seed(sid, &[], true).unwrap();
        let replacement = vec![ConversationMessage::Chat(ChatMessage::user("replacement"))];
        store
            .replace_messages_and_breadcrumb(sid, &replacement, false)
            .unwrap();
        let restored = store.load_session(sid).unwrap().unwrap();
        assert!(restored.retained_context.is_none());
        assert!(!restored.trim_breadcrumb);
        assert_eq!(
            serde_json::to_value(restored.messages).unwrap(),
            serde_json::to_value(replacement).unwrap()
        );
        store.set_trim_breadcrumb(sid, true).unwrap();
        store.replace_messages(sid, &[]).unwrap();
        assert_eq!(raw_trim_breadcrumb_column(&store, sid), Some(1));
    }

    /// Read the raw `trim_breadcrumb` column, bypassing the inference
    /// fallback, so a test can tell `NULL` (never recorded) apart from an
    /// explicit `0`/`1`.
    fn raw_trim_breadcrumb_column(store: &AcpSessionStore, session_uuid: &str) -> Option<i64> {
        store
            .conn
            .lock()
            .query_row(
                "SELECT trim_breadcrumb FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn new_creates_all_tables() {
        let (_tmp, store) = open_store();
        let conn = store.conn.lock();
        for table in [
            "acp_sessions",
            "acp_messages",
            "acp_tool_calls",
            "acp_session_events",
            "acp_turn_checkpoints",
            "acp_turn_checkpoint_events",
        ] {
            let name: String = conn
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| panic!("table {table} should exist"));
            assert_eq!(name, table);
        }
        // A fresh database carries the projected-count cache columns, added
        // idempotently at open and never scored there.
        let mut stmt = conn.prepare("PRAGMA table_info(acp_sessions)").unwrap();
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(stmt);
        for column in ["projected_message_count", "projected_count_through"] {
            assert!(
                columns.contains(&column.to_string()),
                "fresh store must carry the {column} cache column"
            );
        }
    }

    #[test]
    fn opens_in_wal_mode_to_avoid_blocking_runtime_threads() {
        let (_tmp, store) = open_store();
        let conn = store.conn.lock();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "ACP session DB must use WAL");
    }

    #[test]
    fn principal_id_roundtrips_through_create_load_and_list() {
        // RFC 7141 F2: the owning principal is persisted on create and read
        // back by load/list; a NULL owner (unscoped/legacy) stays NULL.
        let (_tmp, store) = open_store();
        store
            .create_session("owned", "alpha", "/ws/o", Some("alice"))
            .unwrap();
        store
            .create_session("unowned", "alpha", "/ws/u", None)
            .unwrap();

        assert_eq!(
            store.load_session("owned").unwrap().unwrap().principal_id,
            Some("alice".to_string()),
        );
        assert_eq!(
            store.load_session("unowned").unwrap().unwrap().principal_id,
            None,
        );
        assert_eq!(
            store.session_owner_and_surface("owned").unwrap(),
            Some((Some("alice".to_string()), None)),
        );
        assert_eq!(
            store.session_owner_and_surface("unowned").unwrap(),
            Some((None, None)),
        );
        assert_eq!(store.session_owner_and_surface("missing").unwrap(), None);
        assert_eq!(
            store
                .load_message_page("owned", 1, None)
                .unwrap()
                .principal_id,
            Some("alice".to_string()),
        );

        let summaries = store.list_sessions().unwrap();
        let owner = |uuid: &str| {
            summaries
                .iter()
                .find(|s| s.session_uuid == uuid)
                .unwrap()
                .principal_id
                .clone()
        };
        assert_eq!(owner("owned"), Some("alice".to_string()));
        assert_eq!(owner("unowned"), None);
    }

    #[test]
    fn session_principal_reports_owner_missing_and_null() {
        let (_tmp, store) = open_store();
        store
            .create_session("owned", "alpha", "/ws/o", Some("alice"))
            .unwrap();
        store
            .create_session("unowned", "alpha", "/ws/u", None)
            .unwrap();

        // Owned -> Some(Some(id)); NULL owner -> Some(None); missing -> None.
        assert_eq!(
            store.session_principal("owned").unwrap(),
            Some(Some("alice".to_string())),
        );
        assert_eq!(store.session_principal("unowned").unwrap(), Some(None));
        assert_eq!(store.session_principal("ghost").unwrap(), None);
    }

    #[test]
    fn principal_index_is_repaired_when_the_column_exists_without_it() {
        // An interrupted first migration can leave the column in place with
        // no index. The migration must not treat "column present" as "done".
        let tmp = TempDir::new().unwrap();
        {
            let store = AcpSessionStore::new(tmp.path()).unwrap();
            store
                .conn
                .lock()
                .execute("DROP INDEX idx_acp_sessions_principal", [])
                .unwrap();
        }
        let reopened = AcpSessionStore::new(tmp.path()).unwrap();
        let indexed: bool = reopened
            .conn
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
                 AND name = 'idx_acp_sessions_principal'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1;
        assert!(
            indexed,
            "reopening must recreate the missing principal index"
        );
    }

    #[test]
    fn principal_id_migration_is_idempotent_across_reopen() {
        // Reopening the same DB re-runs ensure_principal_id_column; it must
        // no-op when the column already exists and preserve stamped owners.
        let tmp = TempDir::new().unwrap();
        {
            let store = AcpSessionStore::new(tmp.path()).unwrap();
            store
                .create_session("s1", "alpha", "/ws", Some("alice"))
                .unwrap();
        }
        let reopened = AcpSessionStore::new(tmp.path()).unwrap();
        assert_eq!(
            reopened.session_principal("s1").unwrap(),
            Some(Some("alice".to_string())),
            "owner must survive a reopen + repeated migration",
        );
    }

    #[test]
    fn create_and_load_session_metadata() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-abc", "personal_code", "/home/user/project", None)
            .unwrap();

        let data = store.load_session("sess-abc").unwrap().unwrap();
        assert_eq!(data.session_uuid, "sess-abc");
        assert_eq!(data.agent_alias, "personal_code");
        assert_eq!(data.workspace_dir, "/home/user/project");
        assert_eq!(data.interaction_surface, None);
        assert_eq!(data.token_count, 0);
        assert!(data.messages.is_empty());
    }

    #[test]
    fn interaction_surface_round_trips_and_legacy_binding_is_one_way() {
        let (_tmp, store) = open_store();
        store
            .create_session_with_interaction_surface(
                "sess-surface",
                "alpha",
                "/tmp/proj",
                Some("zerocode_code"),
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .load_session("sess-surface")
                .unwrap()
                .unwrap()
                .interaction_surface
                .as_deref(),
            Some("zerocode_code")
        );

        store
            .create_session("sess-legacy", "alpha", "/tmp/proj", None)
            .unwrap();
        assert_eq!(
            store
                .bind_interaction_surface_if_unset("sess-legacy", "zerocode_code")
                .unwrap(),
            "zerocode_code"
        );
        assert_eq!(
            store
                .bind_interaction_surface_if_unset("sess-legacy", "different_surface")
                .unwrap(),
            "zerocode_code",
            "a later caller must not relabel an already-bound session"
        );
    }

    #[test]
    fn load_nonexistent_session_returns_none() {
        let (_tmp, store) = open_store();
        assert!(store.load_session("nonexistent").unwrap().is_none());
    }

    #[test]
    fn set_and_get_plan_round_trips() {
        use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};
        let (_tmp, store) = open_store();
        store
            .create_session("sess-plan", "alpha", "/tmp/proj", None)
            .unwrap();

        // No plan yet → empty.
        assert!(store.get_plan("sess-plan").unwrap().is_empty());

        let plan = vec![
            PlanEntry {
                content: "A".to_string(),
                status: PlanStatus::Completed,
                priority: PlanPriority::High,
                active_form: None,
            },
            PlanEntry {
                content: "B".to_string(),
                status: PlanStatus::InProgress,
                priority: PlanPriority::Medium,
                active_form: Some("Doing B".to_string()),
            },
        ];
        store.set_plan("sess-plan", &plan).unwrap();
        assert_eq!(store.get_plan("sess-plan").unwrap(), plan);

        // Whole-list replace: empty slice clears.
        store.set_plan("sess-plan", &[]).unwrap();
        assert!(store.get_plan("sess-plan").unwrap().is_empty());
    }

    #[test]
    fn get_plan_empty_for_unknown_session() {
        let (_tmp, store) = open_store();
        assert!(store.get_plan("nope").unwrap().is_empty());
    }

    #[test]
    fn set_plan_errors_for_unknown_session() {
        let (_tmp, store) = open_store();
        assert!(store.set_plan("nope", &[]).is_err());
    }

    #[test]
    fn append_turn_round_trips_chat_messages() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-msgs", "alpha", "/tmp/proj", None)
            .unwrap();

        let msgs = vec![
            ConversationMessage::Chat(ChatMessage::user("hello")),
            ConversationMessage::Chat(ChatMessage::assistant("hi")),
        ];
        store.append_turn("sess-msgs", &msgs).unwrap();

        let data = store.load_session("sess-msgs").unwrap().unwrap();
        assert_eq!(data.messages.len(), 2);
        assert!(matches!(
            &data.messages[0],
            ConversationMessage::Chat(m) if m.role == "user" && m.content == "hello"
        ));
        assert!(matches!(
            &data.messages[1],
            ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "hi"
        ));
    }

    /// Force a session's message rows and active checkpoint start onto
    /// known, distinct times so ordering assertions do not race the clock.
    fn backdate_checkpoint_start(store: &AcpSessionStore, session_uuid: &str, at: &str) {
        let conn = store.conn.lock();
        let changed = conn
            .execute(
                "UPDATE acp_turn_checkpoints SET started_at = ?1
                  WHERE session_id = (SELECT id FROM acp_sessions WHERE session_uuid = ?2)",
                params![at, session_uuid],
            )
            .unwrap();
        assert_eq!(changed, 1, "session must have an active checkpoint");
    }

    #[test]
    fn finalized_turn_prompt_keeps_turn_start_time_and_reply_keeps_finalization_time() {
        let (_tmp, store) = open_store();
        let sid = "times-finalize";
        store
            .create_session(sid, "default", "/tmp/workspace", None)
            .unwrap();
        let prompt = vec![ConversationMessage::Chat(ChatMessage::user("question"))];
        store.begin_turn_checkpoint(sid, "turn-1", &prompt).unwrap();
        let started = "2026-10-08T17:00:00+00:00";
        backdate_checkpoint_start(&store, sid, started);

        let terminal = vec![
            ConversationMessage::Chat(ChatMessage::user("question")),
            ConversationMessage::AssistantToolCalls {
                text: Some("checking".into()),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "call-1".into(),
                content: "ok".into(),
                tool_name: "shell".into(),
            }]),
            ConversationMessage::Chat(ChatMessage::assistant("done")),
        ];
        store
            .finalize_turn_checkpoint_with_context(sid, "turn-1", &terminal, &terminal, false)
            .unwrap();

        let data = store.load_session(sid).unwrap().unwrap();
        assert_eq!(data.messages.len(), data.message_created_at.len());
        assert_eq!(data.message_created_at[0].as_deref(), Some(started));
        let finalized = data.message_created_at[1].clone().expect("reply time");
        assert_ne!(finalized, started, "only the prompt keeps the turn start");
        assert!(
            data.message_created_at[1..]
                .iter()
                .all(|at| at.as_deref() == Some(&finalized)),
            "the rest of the turn shares the finalization time: {:?}",
            data.message_created_at
        );

        // A cursor page projects narration apart from its calls, so it can
        // hold more messages than a full load; its times stay aligned and
        // carry the same prompt/finalization split.
        let page = store.load_message_page(sid, 10, None).unwrap();
        assert_eq!(page.messages.len(), page.message_created_at.len());
        assert_eq!(page.message_created_at[0].as_deref(), Some(started));
        assert!(
            page.message_created_at[1..]
                .iter()
                .all(|at| at.as_deref() == Some(&finalized)),
            "{:?}",
            page.message_created_at
        );
        let newest = store.load_message_page(sid, 1, None).unwrap();
        assert_eq!(newest.message_created_at, vec![Some(finalized.clone())]);
        let older = store
            .load_message_page(sid, 10, newest.next_cursor.as_deref())
            .unwrap();
        assert_eq!(older.message_created_at[0].as_deref(), Some(started));
    }

    #[test]
    fn recovered_turn_prompt_keeps_turn_start_time() {
        let (_tmp, store) = open_store();
        let sid = "times-recover";
        store
            .create_session(sid, "default", "/tmp/workspace", None)
            .unwrap();
        let prompt = vec![ConversationMessage::Chat(ChatMessage::user("question"))];
        store.begin_turn_checkpoint(sid, "turn-1", &prompt).unwrap();
        let started = "2026-10-08T17:00:00+00:00";
        backdate_checkpoint_start(&store, sid, started);
        store
            .append_turn_checkpoint(
                sid,
                "turn-1",
                &[ConversationMessage::Chat(ChatMessage::assistant("partial"))],
            )
            .unwrap();
        assert!(store.recover_turn_checkpoint(sid, "interrupted").unwrap());

        let data = store.load_session(sid).unwrap().unwrap();
        assert_eq!(data.messages.len(), 3);
        assert_eq!(data.message_created_at[0].as_deref(), Some(started));
        assert_ne!(data.message_created_at[1].as_deref(), Some(started));
    }

    #[test]
    fn turn_checkpoint_start_column_is_added_to_an_existing_database() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE acp_sessions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     session_uuid TEXT NOT NULL UNIQUE,
                     agent_alias TEXT NOT NULL,
                     workspace_dir TEXT NOT NULL,
                     token_count INTEGER NOT NULL DEFAULT 0,
                     created_at TEXT NOT NULL,
                     last_activity TEXT NOT NULL
                 );
                 CREATE TABLE acp_turn_checkpoints (
                     session_id INTEGER PRIMARY KEY REFERENCES acp_sessions(id) ON DELETE CASCADE,
                     turn_id TEXT NOT NULL
                 );",
            )
            .unwrap();
        }
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        {
            let conn = store.conn.lock();
            let columns = conn
                .prepare("PRAGMA table_info(acp_turn_checkpoints)")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(columns, ["session_id", "turn_id", "started_at"]);
        }
        store
            .create_session("legacy", "default", "/tmp", None)
            .unwrap();
        store
            .begin_turn_checkpoint(
                "legacy",
                "turn-1",
                &[ConversationMessage::Chat(ChatMessage::user("q"))],
            )
            .unwrap();
        store
            .finalize_turn_checkpoint_with_context(
                "legacy",
                "turn-1",
                &[
                    ConversationMessage::Chat(ChatMessage::user("q")),
                    ConversationMessage::Chat(ChatMessage::assistant("a")),
                ],
                &[],
                false,
            )
            .unwrap();
        let data = store.load_session("legacy").unwrap().unwrap();
        assert!(data.message_created_at.iter().all(Option::is_some));
    }

    #[test]
    fn interrupted_checkpoint_recovers_once_with_marker() {
        let (_tmp, store) = open_store();
        store
            .create_session("checkpoint-session", "default", "/tmp/workspace", None)
            .unwrap();
        let initial = vec![ConversationMessage::Chat(ChatMessage::user("question"))];
        store
            .begin_turn_checkpoint("checkpoint-session", "turn-1", &initial)
            .unwrap();
        store
            .append_turn_checkpoint(
                "checkpoint-session",
                "turn-1",
                &[ConversationMessage::Chat(ChatMessage::assistant(
                    "partial ",
                ))],
            )
            .unwrap();
        store
            .append_turn_checkpoint(
                "checkpoint-session",
                "turn-1",
                &[ConversationMessage::Chat(ChatMessage::assistant("answer"))],
            )
            .unwrap();

        assert!(
            store
                .recover_turn_checkpoint("checkpoint-session", "stream interrupted")
                .unwrap()
        );
        assert!(
            !store
                .recover_turn_checkpoint("checkpoint-session", "stream interrupted")
                .unwrap()
        );
        let restored = store.load_session("checkpoint-session").unwrap().unwrap();
        assert!(matches!(
            &restored.messages[..],
            [
                ConversationMessage::Chat(user),
                ConversationMessage::Chat(assistant),
                ConversationMessage::Chat(marker),
            ] if user.role == "user"
                && assistant.content == "partial answer"
                && marker.role == "system"
                && marker.content == "stream interrupted"
        ));
        // Cursor history must expose the same client-visible role as a full
        // reload, even when the boundary is the only entry in the newest page.
        let newest = store
            .load_message_page("checkpoint-session", 1, None)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&newest.messages).unwrap(),
            serde_json::to_value(&restored.messages[2..]).unwrap()
        );
        let older = store
            .load_message_page("checkpoint-session", 2, newest.next_cursor.as_deref())
            .unwrap();
        assert_eq!(
            serde_json::to_value(&older.messages).unwrap(),
            serde_json::to_value(&restored.messages[..2]).unwrap()
        );
        assert!(!older.has_older);
        assert!(AcpSessionStore::provider_safe_history(&newest.messages).is_empty());
    }

    #[test]
    fn interruption_boundaries_restore_transcript_and_provider_safe_history() {
        struct Case {
            name: &'static str,
            fragments: Vec<ConversationMessage>,
            expected_transcript_tool_counts: (usize, usize),
            expected_tool_exchange: bool,
        }

        let tool_call = || ToolCall {
            id: "call-1".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"pwd"}"#.to_string(),
            extra_content: None,
        };
        let assistant = ConversationMessage::Chat(ChatMessage::assistant("checking"));
        let call = ConversationMessage::AssistantToolCalls {
            text: None,
            tool_calls: vec![tool_call()],
            reasoning_content: None,
        };
        let result = ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "call-1".to_string(),
            tool_name: "shell".to_string(),
            content: "/tmp/workspace".to_string(),
        }]);
        let cases = [
            Case {
                name: "assistant-text",
                fragments: vec![assistant.clone()],
                expected_transcript_tool_counts: (0, 0),
                expected_tool_exchange: false,
            },
            Case {
                name: "tool-call",
                fragments: vec![assistant.clone(), call.clone()],
                expected_transcript_tool_counts: (1, 0),
                expected_tool_exchange: false,
            },
            Case {
                name: "tool-result",
                fragments: vec![assistant, call, result],
                expected_transcript_tool_counts: (1, 1),
                expected_tool_exchange: true,
            },
        ];

        for case in cases {
            let (_tmp, store) = open_store();
            let session_id = format!("checkpoint-{}", case.name);
            store
                .create_session(&session_id, "default", "/tmp/workspace", None)
                .unwrap();
            store
                .begin_turn_checkpoint(
                    &session_id,
                    "turn-1",
                    &[ConversationMessage::Chat(ChatMessage::user("question"))],
                )
                .unwrap();
            for fragment in case.fragments {
                store
                    .append_turn_checkpoint(&session_id, "turn-1", &[fragment])
                    .unwrap();
            }

            assert!(
                store
                    .recover_turn_checkpoint(&session_id, "stream interrupted")
                    .unwrap(),
                "{} checkpoint should recover",
                case.name
            );
            let restored = store.load_session(&session_id).unwrap().unwrap();
            assert!(
                matches!(
                    restored.messages.last(),
                    Some(ConversationMessage::Chat(marker))
                        if marker.role == "system" && marker.content == "stream interrupted"
                ),
                "{} transcript should end with the interruption marker",
                case.name
            );
            assert!(
                restored.messages.iter().any(|message| matches!(
                    message,
                    ConversationMessage::Chat(chat)
                        if chat.role == "assistant" && chat.content == "checking"
                ) || matches!(
                    message,
                    ConversationMessage::AssistantToolCalls { text: Some(text), .. }
                        if text == "checking"
                )),
                "{} transcript should retain visible assistant text",
                case.name
            );
            let transcript_tool_counts =
                restored
                    .messages
                    .iter()
                    .fold((0, 0), |(calls, results), message| match message {
                        ConversationMessage::AssistantToolCalls { .. } => (calls + 1, results),
                        ConversationMessage::ToolResults(_) => (calls, results + 1),
                        ConversationMessage::Chat(_) => (calls, results),
                    });
            assert_eq!(
                transcript_tool_counts, case.expected_transcript_tool_counts,
                "{} transcript should retain every visible tool boundary",
                case.name
            );

            let provider_history = AcpSessionStore::provider_safe_history(&restored.messages);
            assert!(
                provider_history.iter().all(|message| !matches!(
                    message,
                    ConversationMessage::Chat(chat) if chat.role == "system"
                )),
                "{} provider history should exclude the interruption marker",
                case.name
            );
            assert!(
                matches!(
                    provider_history.first(),
                    Some(ConversationMessage::Chat(user))
                        if user.role == "user" && user.content == "question"
                ),
                "{} provider history should retain the accepted prompt",
                case.name
            );
            assert!(
                provider_history.iter().any(|message| matches!(
                    message,
                    ConversationMessage::Chat(chat)
                        if chat.role == "assistant" && chat.content == "checking"
                ) || matches!(
                    message,
                    ConversationMessage::AssistantToolCalls { text: Some(text), .. }
                        if text == "checking"
                )),
                "{} provider history should retain visible assistant text",
                case.name
            );
            let tool_call_count = provider_history
                .iter()
                .filter(|message| matches!(message, ConversationMessage::AssistantToolCalls { .. }))
                .count();
            let tool_result_count = provider_history
                .iter()
                .filter(|message| matches!(message, ConversationMessage::ToolResults(_)))
                .count();
            assert_eq!(
                (tool_call_count, tool_result_count),
                if case.expected_tool_exchange {
                    (1, 1)
                } else {
                    (0, 0)
                },
                "{} provider history should contain only a complete tool exchange",
                case.name
            );
        }
    }

    #[test]
    fn missing_session_has_no_checkpoint_to_recover() {
        let (_tmp, store) = open_store();

        assert!(
            !store
                .recover_turn_checkpoint("missing-session", "stream interrupted")
                .unwrap()
        );
    }

    #[test]
    fn checkpoint_turn_id_mismatch_cannot_append_or_finalize() {
        let (_tmp, store) = open_store();
        store
            .create_session("checkpoint-identity", "default", "/tmp/workspace", None)
            .unwrap();
        let messages = vec![ConversationMessage::Chat(ChatMessage::user("question"))];
        store
            .begin_turn_checkpoint("checkpoint-identity", "turn-current", &messages)
            .unwrap();

        assert!(
            store
                .append_turn_checkpoint("checkpoint-identity", "turn-stale", &messages)
                .is_err()
        );
        assert!(
            store
                .finalize_turn_checkpoint("checkpoint-identity", "turn-stale", &messages)
                .is_err()
        );
        assert!(
            store
                .recover_turn_checkpoint("checkpoint-identity", "interrupted")
                .unwrap()
        );
    }

    #[test]
    fn failed_checkpoint_finalization_preserves_recoverable_fragments() {
        let (_tmp, store) = open_store();
        store
            .create_session("checkpoint-finalize", "default", "/tmp/workspace", None)
            .unwrap();
        let initial = vec![ConversationMessage::Chat(ChatMessage::user("question"))];
        store
            .begin_turn_checkpoint("checkpoint-finalize", "turn-1", &initial)
            .unwrap();
        store
            .append_turn_checkpoint(
                "checkpoint-finalize",
                "turn-1",
                &[ConversationMessage::Chat(ChatMessage::assistant("partial"))],
            )
            .unwrap();

        let invalid_terminal = vec![ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "missing".to_string(),
            tool_name: "shell".to_string(),
            content: "orphan".to_string(),
        }])];
        assert!(
            store
                .finalize_turn_checkpoint("checkpoint-finalize", "turn-1", &invalid_terminal,)
                .is_err()
        );
        assert!(
            store
                .recover_turn_checkpoint("checkpoint-finalize", "interrupted")
                .unwrap()
        );
        let restored = store.load_session("checkpoint-finalize").unwrap().unwrap();
        assert!(matches!(
            &restored.messages[..],
            [
                ConversationMessage::Chat(user),
                ConversationMessage::Chat(assistant),
                ConversationMessage::Chat(marker),
            ] if user.role == "user"
                && assistant.content == "partial"
                && marker.role == "system"
        ));
    }

    #[test]
    fn killed_session_checkpoint_is_not_promoted() {
        let (_tmp, store) = open_store();
        store
            .create_session("checkpoint-killed", "default", "/tmp/workspace", None)
            .unwrap();
        let initial = [ConversationMessage::Chat(ChatMessage::user("question"))];
        store
            .begin_turn_checkpoint("checkpoint-killed", "turn-1", &initial)
            .unwrap();
        assert!(store.mark_session_killed("checkpoint-killed").unwrap());

        assert!(
            !store
                .recover_turn_checkpoint("checkpoint-killed", "interrupted")
                .unwrap()
        );
        assert!(
            store
                .load_session("checkpoint-killed")
                .unwrap()
                .unwrap()
                .messages
                .is_empty()
        );
        assert!(
            store
                .append_turn_checkpoint("checkpoint-killed", "turn-1", &[])
                .is_ok(),
            "the untouched checkpoint should retain its turn identity"
        );
    }

    #[test]
    fn provider_safe_history_requires_adjacent_exact_tool_pairs() {
        let messages = vec![
            ConversationMessage::Chat(ChatMessage::system("interrupted")),
            ConversationMessage::AssistantToolCalls {
                text: Some("partial".to_string()),
                tool_calls: vec![
                    ToolCall {
                        id: "paired".to_string(),
                        name: "shell".to_string(),
                        arguments: "{}".to_string(),
                        extra_content: None,
                    },
                    ToolCall {
                        id: "orphan".to_string(),
                        name: "shell".to_string(),
                        arguments: "{}".to_string(),
                        extra_content: None,
                    },
                ],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "paired".to_string(),
                tool_name: "shell".to_string(),
                content: "first".to_string(),
            }]),
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "orphan".to_string(),
                tool_name: "shell".to_string(),
                content: "misplaced".to_string(),
            }]),
        ];

        let repaired = AcpSessionStore::provider_safe_history(&messages);
        assert_eq!(repaired.len(), 2);
        assert!(matches!(
            &repaired[0],
            ConversationMessage::AssistantToolCalls { text, tool_calls, .. }
                if text.as_deref() == Some("partial")
                    && tool_calls.len() == 1
                    && tool_calls[0].id == "paired"
        ));
        assert!(matches!(
            &repaired[1],
            ConversationMessage::ToolResults(results)
                if results.len() == 1
                    && results[0].tool_call_id == "paired"
                    && results[0].content == "first"
        ));
    }

    #[test]
    fn provider_safe_history_rejects_ambiguous_tool_ids_per_batch() {
        let call = |id: &str| ToolCall {
            id: id.into(),
            name: "shell".into(),
            arguments: "{}".into(),
            extra_content: None,
        };
        let result = |id: &str| ToolResultMessage {
            tool_call_id: id.into(),
            tool_name: "shell".into(),
            content: format!("output for {id}"),
        };
        for (call_count, result_count) in [(2, 1), (2, 2), (1, 2)] {
            for keep_unique_peer in [false, true] {
                let mut calls = vec![call("duplicate"); call_count];
                let mut results = vec![result("duplicate"); result_count];
                if keep_unique_peer {
                    calls.push(call("unique"));
                    results.push(result("unique"));
                }
                let messages = vec![
                    ConversationMessage::AssistantToolCalls {
                        text: Some("partial text".into()),
                        tool_calls: calls,
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(results),
                ];
                let mut expected = if keep_unique_peer {
                    vec![
                        ConversationMessage::AssistantToolCalls {
                            text: Some("partial text".into()),
                            tool_calls: vec![call("unique")],
                            reasoning_content: None,
                        },
                        ConversationMessage::ToolResults(vec![result("unique")]),
                    ]
                } else {
                    vec![ConversationMessage::Chat(ChatMessage::assistant(
                        "partial text",
                    ))]
                };
                // Reusing the ID in a later, unambiguous batch remains valid.
                let later = vec![
                    ConversationMessage::AssistantToolCalls {
                        text: None,
                        tool_calls: vec![call("duplicate")],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![result("duplicate")]),
                ];
                let mut messages = messages;
                messages.extend(later.clone());
                expected.extend(later);
                let original = serde_json::to_value(&messages).unwrap();
                assert_eq!(
                    serde_json::to_value(AcpSessionStore::provider_safe_history(&messages))
                        .unwrap(),
                    serde_json::to_value(expected).unwrap(),
                    "call count={call_count}, result count={result_count}, unique peer={keep_unique_peer}"
                );
                assert_eq!(serde_json::to_value(&messages).unwrap(), original);
            }
        }
    }

    #[test]
    fn bounded_transcript_messages_caps_terminal_tool_output() {
        let messages = [
            ConversationMessage::AssistantToolCalls {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "shell".to_string(),
                    arguments: "{}".to_string(),
                    extra_content: None,
                }],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "call-1".to_string(),
                tool_name: "shell".to_string(),
                content: "x".repeat(MAX_PERSISTED_TOOL_OUTPUT_BYTES + 10),
            }]),
        ];
        let bounded = AcpSessionStore::bounded_transcript_messages(&messages);
        assert!(matches!(
            &bounded[1],
            ConversationMessage::ToolResults(results)
                if results[0].content.ends_with("…[truncated]")
                    && results[0].content.len()
                        <= MAX_PERSISTED_TOOL_OUTPUT_BYTES + "…[truncated]".len()
        ));

        let (_tmp, store) = open_store();
        store
            .create_session("bounded-terminal", "default", "/tmp/workspace", None)
            .unwrap();
        store.append_turn("bounded-terminal", &messages).unwrap();
        let restored = store.load_session("bounded-terminal").unwrap().unwrap();
        assert!(matches!(
            &restored.messages[1],
            ConversationMessage::ToolResults(results)
                if results[0].content.ends_with("…[truncated]")
        ));
    }

    #[test]
    fn replace_messages_drops_prior_rows_and_cascades_to_tool_calls() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-replace", "alpha", "/tmp/proj", None)
            .unwrap();

        // An existing turn with a tool call, to prove the old row (and its
        // cascaded acp_tool_calls row) is fully gone after replace, not left
        // behind alongside the new transcript.
        let old = vec![
            ConversationMessage::AssistantToolCalls {
                text: Some(String::new()),
                tool_calls: vec![zeroclaw_api::model_provider::ToolCall {
                    id: "call-1".into(),
                    name: "old_tool".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![
                zeroclaw_api::model_provider::ToolResultMessage {
                    tool_call_id: "call-1".into(),
                    content: "old result".into(),
                    tool_name: "old_tool".into(),
                },
            ]),
        ];
        store.append_turn("sess-replace", &old).unwrap();
        assert_eq!(
            store
                .load_session("sess-replace")
                .unwrap()
                .unwrap()
                .messages
                .len(),
            2
        );

        let new = vec![
            ConversationMessage::Chat(ChatMessage::user("hello")),
            ConversationMessage::Chat(ChatMessage::assistant("hi")),
        ];
        store.replace_messages("sess-replace", &new).unwrap();

        let data = store.load_session("sess-replace").unwrap().unwrap();
        assert_eq!(
            data.messages.len(),
            2,
            "replace must not leave the prior turn's rows behind"
        );
        assert!(matches!(
            &data.messages[0],
            ConversationMessage::Chat(m) if m.role == "user" && m.content == "hello"
        ));
        assert!(matches!(
            &data.messages[1],
            ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "hi"
        ));

        // A fresh call, unrelated to the replaced-away "call-1", must not
        // resolve tool_name off the deleted (cascaded) tool_calls row.
        store
            .append_turn(
                "sess-replace",
                &[
                    ConversationMessage::AssistantToolCalls {
                        text: Some(String::new()),
                        tool_calls: vec![zeroclaw_api::model_provider::ToolCall {
                            id: "call-2".into(),
                            name: "new_tool".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        }],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![
                        zeroclaw_api::model_provider::ToolResultMessage {
                            tool_call_id: "call-2".into(),
                            content: "new result".into(),
                            tool_name: "new_tool".into(),
                        },
                    ]),
                ],
            )
            .unwrap();
        let data = store.load_session("sess-replace").unwrap().unwrap();
        assert_eq!(data.messages.len(), 4);
    }

    #[test]
    fn replace_paths_reset_the_projected_count_to_the_new_transcript() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-count-replace", "alpha", "/tmp/proj", None)
            .unwrap();

        // A tool-loop turn whose projected count (5) exceeds its chat rows:
        // one chat entry, a batch contributing its text plus two call
        // entries, both results folding into open calls, one more chat.
        let seeded = vec![
            ConversationMessage::Chat(ChatMessage::user("fold me")),
            ConversationMessage::AssistantToolCalls {
                text: Some("batch".into()),
                tool_calls: vec![
                    ToolCall {
                        id: "c1".into(),
                        name: "shell".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                    ToolCall {
                        id: "c2".into(),
                        name: "read".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                ],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![
                ToolResultMessage {
                    tool_call_id: "c1".into(),
                    content: "ok".into(),
                    tool_name: "shell".into(),
                },
                ToolResultMessage {
                    tool_call_id: "c2".into(),
                    content: "ok".into(),
                    tool_name: "read".into(),
                },
            ]),
            ConversationMessage::Chat(ChatMessage::user("orphan")),
        ];
        store.append_turn("sess-count-replace", &seeded).unwrap();
        assert_eq!(projected_entry_count(&seeded), 5);
        assert_eq!(
            store.projected_message_count("sess-count-replace").unwrap(),
            Some(5)
        );

        // A trim-driven replace swaps the transcript for two chat turns: the
        // persisted counter must follow the new transcript, not keep the
        // pre-trim value or grow by the replaced batch.
        store
            .replace_messages(
                "sess-count-replace",
                &[
                    ConversationMessage::Chat(ChatMessage::user("kept")),
                    ConversationMessage::Chat(ChatMessage::assistant("kept answer")),
                ],
            )
            .unwrap();
        assert_eq!(
            store.projected_message_count("sess-count-replace").unwrap(),
            Some(2)
        );

        // The breadcrumb variant carries the same contract.
        store
            .replace_messages_and_breadcrumb(
                "sess-count-replace",
                &[
                    ConversationMessage::Chat(ChatMessage::user("trimmed")),
                    ConversationMessage::Chat(ChatMessage::assistant("trimmed answer")),
                    ConversationMessage::Chat(ChatMessage::user("next")),
                ],
                true,
            )
            .unwrap();
        assert_eq!(
            store.projected_message_count("sess-count-replace").unwrap(),
            Some(3)
        );

        // Appends after a replace build on the replaced base, so the store
        // and the runtime projection stay in step across a trim.
        store
            .append_turn(
                "sess-count-replace",
                &[ConversationMessage::Chat(ChatMessage::user("more"))],
            )
            .unwrap();
        assert_eq!(
            store.projected_message_count("sess-count-replace").unwrap(),
            Some(4)
        );
        assert_eq!(raw_count_pair(&store, "sess-count-replace"), (4, Some(9)));
    }

    #[test]
    fn insert_messages_never_persists_a_system_row() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-system", "alpha", "/tmp/proj", None)
            .unwrap();

        // An agent's authoritative `history()` always leads with the system
        // prompt. Both write paths must drop it, since durable ACP rows
        // become `session/messages` API output with no restore-side filter.
        let full_history = vec![
            ConversationMessage::Chat(ChatMessage::system("you are a helpful agent")),
            ConversationMessage::Chat(ChatMessage::user("hello")),
            ConversationMessage::Chat(ChatMessage::assistant("hi")),
        ];
        store
            .replace_messages_and_breadcrumb("sess-system", &full_history, false)
            .unwrap();

        let data = store.load_session("sess-system").unwrap().unwrap();
        assert_eq!(
            data.messages.len(),
            2,
            "the system row must not be persisted"
        );
        assert!(
            data.messages
                .iter()
                .all(|m| !matches!(m, ConversationMessage::Chat(c) if c.role == "system")),
        );

        // append_turn shares the same insertion path.
        store
            .append_turn(
                "sess-system",
                &[ConversationMessage::Chat(ChatMessage::system(
                    "a later system message",
                ))],
            )
            .unwrap();
        let data = store.load_session("sess-system").unwrap().unwrap();
        assert_eq!(
            data.messages.len(),
            2,
            "append_turn must skip system rows too"
        );
    }

    #[test]
    fn load_session_filters_a_legacy_system_row_written_before_the_write_path_fix() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-legacy-system", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .append_turn(
                "sess-legacy-system",
                &[ConversationMessage::Chat(ChatMessage::user("hello"))],
            )
            .unwrap();

        // Simulate a row written before `insert_messages` filtered system
        // rows: insert one directly, bypassing every write path.
        {
            let conn = store.conn.lock();
            let session_id: i64 = conn
                .query_row(
                    "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
                    params!["sess-legacy-system"],
                    |row| row.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO acp_messages (session_id, role, content, created_at)
                 VALUES (?1, 'system', 'legacy stored prompt', '2020-01-01T00:00:00Z')",
                params![session_id],
            )
            .unwrap();
        }

        let data = store.load_session("sess-legacy-system").unwrap().unwrap();
        assert!(
            data.messages
                .iter()
                .all(|m| !matches!(m, ConversationMessage::Chat(c) if c.role == "system")),
            "a legacy system row must not reach session/messages output even though \
             it predates the write-path filter: {:?}",
            data.messages
        );

        let restored = store
            .load_session_for_restore("sess-legacy-system")
            .unwrap();
        let AcpSessionRestore::Restorable(restored) = restored else {
            panic!("expected a restorable session");
        };
        assert!(
            restored
                .messages
                .iter()
                .all(|m| !matches!(m, ConversationMessage::Chat(c) if c.role == "system")),
            "restore must not resurrect a legacy system row into the Agent's history either"
        );
    }

    #[test]
    fn replace_messages_unknown_session_errors() {
        let (_tmp, store) = open_store();
        let msgs = vec![ConversationMessage::Chat(ChatMessage::user("hi"))];
        assert!(store.replace_messages("no-such-session", &msgs).is_err());
    }

    #[test]
    fn append_turn_decomposes_assistant_tool_calls_and_results() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-variants", "alpha", "/tmp/proj", None)
            .unwrap();

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
                reasoning_content: Some("think think".into()),
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc-1".into(),
                content: "file.txt\n".into(),
                tool_name: String::new(),
            }]),
            ConversationMessage::Chat(ChatMessage::assistant("done")),
        ];
        store.append_turn("sess-variants", &msgs).unwrap();

        let data = store.load_session("sess-variants").unwrap().unwrap();
        assert_eq!(data.messages.len(), 4);

        // Round-trip: AssistantToolCalls preserves text + tool_calls + reasoning
        match &data.messages[1] {
            ConversationMessage::AssistantToolCalls {
                text,
                tool_calls,
                reasoning_content,
            } => {
                assert_eq!(text.as_deref(), Some("calling shell"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "tc-1");
                assert_eq!(tool_calls[0].name, "shell");
                assert_eq!(tool_calls[0].arguments, r#"{"command":"ls"}"#);
                assert_eq!(reasoning_content.as_deref(), Some("think think"));
            }
            other => panic!("expected AssistantToolCalls, got {other:?}"),
        }

        // Round-trip: ToolResults preserves tool_call_id + content
        match &data.messages[2] {
            ConversationMessage::ToolResults(results) => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].tool_call_id, "tc-1");
                assert_eq!(results[0].content, "file.txt\n");
            }
            other => panic!("expected ToolResults, got {other:?}"),
        }
    }

    #[test]
    fn no_data_duplication_tool_call_payload_only_in_tool_calls_table() {
        // The schema contract: tool-call args and results live ONLY in
        // acp_tool_calls. The assistant's message row carries only the text.
        let (_tmp, store) = open_store();
        store
            .create_session("sess-dup", "alpha", "/tmp/proj", None)
            .unwrap();

        store
            .append_turn(
                "sess-dup",
                &[ConversationMessage::AssistantToolCalls {
                    text: Some("running".into()),
                    tool_calls: vec![ToolCall {
                        id: "tc-x".into(),
                        name: "shell".into(),
                        arguments: r#"{"command":"echo hi"}"#.into(),
                        extra_content: None,
                    }],
                    reasoning_content: None,
                }],
            )
            .unwrap();

        let conn = store.conn.lock();
        let msg_content: String = conn
            .query_row(
                "SELECT content FROM acp_messages WHERE role = 'assistant' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_content, "running");
        assert!(
            !msg_content.contains("echo hi"),
            "message content must not contain tool-call args"
        );
    }

    #[test]
    fn append_turn_empty_slice_is_noop() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-empty", "alpha", "/tmp/proj", None)
            .unwrap();
        store.append_turn("sess-empty", &[]).unwrap();
        let data = store.load_session("sess-empty").unwrap().unwrap();
        assert!(data.messages.is_empty());
    }

    #[test]
    fn append_turn_skips_zero_entry_tool_call_batches() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-degenerate", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .append_turn(
                "sess-degenerate",
                &[
                    ConversationMessage::Chat(ChatMessage::user("first")),
                    // Degenerate zero-entry batches: no text, no calls. The
                    // counting contract scores them zero entries, so nothing
                    // may be persisted for them; a stored row would reload
                    // as a plain chat message and project as 1.
                    ConversationMessage::AssistantToolCalls {
                        text: None,
                        tool_calls: vec![],
                        reasoning_content: None,
                    },
                    ConversationMessage::AssistantToolCalls {
                        text: Some(String::new()),
                        tool_calls: vec![],
                        reasoning_content: None,
                    },
                    ConversationMessage::Chat(ChatMessage::user("second")),
                ],
            )
            .unwrap();

        let summary = &store.list_sessions().unwrap()[0];
        assert_eq!(summary.message_count, 2);
        let data = store.load_session("sess-degenerate").unwrap().unwrap();
        assert_eq!(data.messages.len(), 2);
        assert_eq!(projected_entry_count(&data.messages), 2);
    }

    #[test]
    fn last_activity_updated_on_append() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-activity", "alpha", "/tmp/proj", None)
            .unwrap();
        let before = store
            .load_session("sess-activity")
            .unwrap()
            .unwrap()
            .last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .append_turn(
                "sess-activity",
                &[ConversationMessage::Chat(ChatMessage::user("hi"))],
            )
            .unwrap();
        let after = store
            .load_session("sess-activity")
            .unwrap()
            .unwrap()
            .last_activity;
        assert!(after >= before);
    }

    #[test]
    fn append_turn_unknown_session_errors_atomically() {
        let (_tmp, store) = open_store();
        let result = store.append_turn(
            "does-not-exist",
            &[ConversationMessage::Chat(ChatMessage::user("hello"))],
        );
        assert!(result.is_err());
        let conn = store.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM acp_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "no orphan rows after failed append_turn");
    }

    #[test]
    fn cursor_excludes_legacy_system_rows_and_terminates() {
        let (_tmp, store) = open_store();
        for (session, rows, expected) in [
            (
                "mixed",
                vec![
                    ("system", "legacy leading prompt"),
                    ("user", "old"),
                    ("system", "legacy middle prompt"),
                    ("assistant", "new"),
                    ("system", "legacy trailing prompt"),
                ],
                vec!["new", "old"],
            ),
            (
                "system-only",
                vec![("system", "legacy only prompt")],
                vec![],
            ),
        ] {
            store
                .create_session(session, "alpha", "/tmp", None)
                .unwrap();
            {
                // Bypass today's write filter to represent an existing database.
                let conn = store.conn.lock();
                let session_id: i64 = conn
                    .query_row(
                        "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
                        params![session],
                        |row| row.get(0),
                    )
                    .unwrap();
                for (role, content) in rows {
                    conn.execute(
                        "INSERT INTO acp_messages (session_id, role, content, created_at)
                         VALUES (?1, ?2, ?3, '2020-01-01T00:00:00Z')",
                        params![session_id, role, content],
                    )
                    .unwrap();
                }
            }

            let mut cursor = None;
            let mut contents = Vec::new();
            for page_index in 0..expected.len().max(1) {
                let page = store
                    .load_message_page(session, 1, cursor.as_deref())
                    .unwrap();
                assert_eq!(page.messages.len(), usize::from(!expected.is_empty()));
                for message in page.messages {
                    let ConversationMessage::Chat(chat) = message else {
                        panic!("expected a plain transcript message");
                    };
                    assert_ne!(chat.role, "system");
                    contents.push(chat.content);
                }
                let has_older = page_index + 1 < expected.len();
                assert_eq!(page.has_older, has_older);
                assert_eq!(page.next_cursor.is_some(), has_older);
                cursor = page.next_cursor;
            }
            assert_eq!(contents, expected);
        }
    }

    #[test]
    fn cursor_pages_newest_rows_and_walks_back_without_repeating() {
        let (_tmp, store) = open_store();
        store
            .create_session("cursor", "alpha", "/tmp", None)
            .unwrap();
        for content in ["old", "middle", "new"] {
            store
                .append_turn(
                    "cursor",
                    &[ConversationMessage::Chat(ChatMessage::assistant(content))],
                )
                .unwrap();
        }

        let first = store.load_message_page("cursor", 2, None).unwrap();
        assert_eq!(first.messages.len(), 2);
        assert!(first.has_older);
        let second = store
            .load_message_page("cursor", 2, first.next_cursor.as_deref())
            .unwrap();
        assert!(!second.has_older);
        let text = |messages: &[ConversationMessage]| {
            messages
                .iter()
                .map(|message| match message {
                    ConversationMessage::Chat(chat) => chat.content.clone(),
                    _ => "non-chat".to_owned(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(text(&first.messages), vec!["middle", "new"]);
        assert_eq!(text(&second.messages), vec!["old"]);
    }

    #[test]
    fn cursor_preserves_pure_chat_roles_including_empty_content() {
        let (_tmp, store) = open_store();
        store
            .create_session("chat-roles", "alpha", "/tmp", None)
            .unwrap();
        store
            .append_turn(
                "chat-roles",
                &[
                    ConversationMessage::Chat(ChatMessage::user("question")),
                    ConversationMessage::Chat(ChatMessage::assistant("answer")),
                    ConversationMessage::Chat(ChatMessage {
                        role: "user".into(),
                        content: String::new(),
                    }),
                ],
            )
            .unwrap();
        let page = store.load_message_page("chat-roles", 3, None).unwrap();
        assert_eq!(page.messages.len(), 3);
        assert!(matches!(
            &page.messages[0],
            ConversationMessage::Chat(chat) if chat.role == "user" && chat.content == "question"
        ));
        assert!(matches!(
            &page.messages[1],
            ConversationMessage::Chat(chat) if chat.role == "assistant" && chat.content == "answer"
        ));
        assert!(matches!(
            &page.messages[2],
            ConversationMessage::Chat(chat) if chat.role == "user" && chat.content.is_empty()
        ));
    }

    #[test]
    fn cursor_pages_tool_group_by_projected_entry_offset() {
        let (_tmp, store) = open_store();
        store
            .create_session("tools", "alpha", "/tmp", None)
            .unwrap();
        store
            .append_turn(
                "tools",
                &[
                    ConversationMessage::AssistantToolCalls {
                        text: Some("narration".into()),
                        tool_calls: vec![
                            ToolCall {
                                id: "one".into(),
                                name: "shell".into(),
                                arguments: "{}".into(),
                                extra_content: None,
                            },
                            ToolCall {
                                id: "two".into(),
                                name: "shell".into(),
                                arguments: "{}".into(),
                                extra_content: None,
                            },
                        ],
                        reasoning_content: Some("tool reasoning".into()),
                    },
                    ConversationMessage::ToolResults(vec![
                        ToolResultMessage {
                            tool_call_id: "two".into(),
                            content: "two-result".into(),
                            tool_name: "shell".into(),
                        },
                        ToolResultMessage {
                            tool_call_id: "orphan".into(),
                            content: "orphan-result".into(),
                            tool_name: "shell".into(),
                        },
                        ToolResultMessage {
                            tool_call_id: "one".into(),
                            content: "one-result".into(),
                            tool_name: "shell".into(),
                        },
                    ]),
                ],
            )
            .unwrap();

        let first = store.load_message_page("tools", 2, None).unwrap();
        assert_eq!(
            first.messages.len(),
            3,
            "two projected entries include paired outputs"
        );
        assert!(first.has_older);
        let second = store
            .load_message_page("tools", 2, first.next_cursor.as_deref())
            .unwrap();
        assert!(!second.has_older);
        assert_eq!(second.messages.len(), 3, "narration plus paired first call");
        for message in first.messages.iter().chain(&second.messages) {
            if let ConversationMessage::AssistantToolCalls {
                reasoning_content, ..
            } = message
            {
                assert_eq!(reasoning_content.as_deref(), Some("tool reasoning"));
            }
        }
    }

    #[test]
    fn cursor_snapshot_excludes_appends_after_initial_page() {
        let (_tmp, store) = open_store();
        store
            .create_session("stable", "alpha", "/tmp", None)
            .unwrap();
        store
            .append_turn(
                "stable",
                &[
                    ConversationMessage::Chat(ChatMessage::assistant("old")),
                    ConversationMessage::Chat(ChatMessage::assistant("middle")),
                ],
            )
            .unwrap();
        let first = store.load_message_page("stable", 1, None).unwrap();
        store
            .append_turn(
                "stable",
                &[ConversationMessage::Chat(ChatMessage::assistant("new"))],
            )
            .unwrap();
        assert!(first.has_older);
        let cursor = first.next_cursor.clone();

        // A new initial request sees the append; the established cursor does
        // not, which is the duplicate/gap-free snapshot guarantee.
        let fresh = store.load_message_page("stable", 1, None).unwrap();
        assert!(matches!(
            &fresh.messages[0],
            ConversationMessage::Chat(message) if message.content == "new"
        ));
        let older = store
            .load_message_page("stable", 1, cursor.as_deref())
            .unwrap();
        assert!(matches!(
            &older.messages[0],
            ConversationMessage::Chat(message) if message.content == "old"
        ));
    }

    #[test]
    fn cursor_rejects_transcript_replacement_and_fresh_request_recovers() {
        let (_tmp, store) = open_store();
        store
            .create_session("replaced", "alpha", "/tmp", None)
            .unwrap();
        let transcript = [
            ConversationMessage::Chat(ChatMessage::user("old")),
            ConversationMessage::Chat(ChatMessage::assistant("middle")),
            ConversationMessage::Chat(ChatMessage::assistant("new")),
        ];
        store.append_turn("replaced", &transcript).unwrap();
        let first = store.load_message_page("replaced", 1, None).unwrap();
        let cursor = first
            .next_cursor
            .expect("three messages must leave an older page");

        // Terminal turns replace the authoritative transcript, assigning new
        // durable row IDs. An older cursor must not report false exhaustion.
        store
            .replace_messages_and_breadcrumb("replaced", &transcript, false)
            .unwrap();
        let error = store
            .load_message_page("replaced", 1, Some(&cursor))
            .expect_err("replacement must invalidate an established cursor");
        assert_eq!(error.to_string(), "invalid ACP session cursor");

        let fresh = store.load_message_page("replaced", 1, None).unwrap();
        assert!(matches!(
            &fresh.messages[0],
            ConversationMessage::Chat(message) if message.content == "new"
        ));
        assert!(fresh.has_older);
    }

    #[test]
    fn cursor_rejects_tool_id_reuse_across_message_groups() {
        let (_tmp, store) = open_store();
        store
            .create_session("reuse", "alpha", "/tmp", None)
            .unwrap();
        let call = |id: &str| ConversationMessage::AssistantToolCalls {
            text: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        };
        let result = |id: &str| {
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: id.into(),
                content: "ok".into(),
                tool_name: "shell".into(),
            }])
        };
        store
            .append_turn("reuse", &[call("same"), result("same")])
            .unwrap();
        store
            .append_turn("reuse", &[call("same"), result("same")])
            .unwrap();
        let error = store
            .load_message_page("reuse", 1, None)
            .expect_err("cross-group ID reuse must fail closed");
        assert!(error.to_string().contains("cross-group"));
    }

    #[test]
    fn cursor_defers_malformed_older_group_until_requested() {
        let (_tmp, store) = open_store();
        store
            .create_session("defer", "alpha", "/tmp", None)
            .unwrap();
        store
            .append_turn(
                "defer",
                &[
                    ConversationMessage::Chat(ChatMessage::assistant("old")),
                    ConversationMessage::Chat(ChatMessage::assistant("new")),
                ],
            )
            .unwrap();
        let conn = store.conn.lock();
        // Add an invalid event to the older row without affecting the newer
        // page; loading that row later must be where the error appears.
        let old_id: i64 = conn
            .query_row(
                "SELECT id FROM acp_messages WHERE content = 'old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls
             (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (?1, 'bad', 'shell', 'bad', 'x', NULL, '2026-01-01T00:00:00Z')",
            params![old_id],
        )
        .unwrap();
        drop(conn);

        let first = store.load_message_page("defer", 1, None).unwrap();
        assert!(first.has_older);
        let error = store
            .load_message_page("defer", 1, first.next_cursor.as_deref())
            .expect_err("malformed group should fail when its page is reached");
        assert!(error.to_string().contains("unknown event_kind"));
    }

    #[test]
    fn cursor_rejects_malformed_group_with_no_valid_projected_entries() {
        let (_tmp, store) = open_store();
        store
            .create_session("bad-empty", "alpha", "/tmp", None)
            .unwrap();
        // `append_turn` persists nothing for a zero-entry tool-call batch, so
        // the empty assistant row a malformed-only group hangs off is written
        // directly, as an older binary or a damaged DB would leave it.
        let conn = store.conn.lock();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES ((SELECT id FROM acp_sessions WHERE session_uuid = 'bad-empty'),
                     'assistant', '', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let message_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO acp_tool_calls
             (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (?1, 'bad', 'shell', 'bad', 'x', NULL, '2026-01-01T00:00:00Z')",
            params![message_id],
        )
        .unwrap();
        drop(conn);
        let error = store
            .load_message_page("bad-empty", 1, None)
            .expect_err("malformed-only group must not disappear");
        assert!(error.to_string().contains("unknown event_kind"));
    }

    #[test]
    fn cursor_rejects_invalid_and_wrong_session_tokens() {
        let (_tmp, store) = open_store();
        store.create_session("one", "alpha", "/tmp", None).unwrap();
        store.create_session("two", "alpha", "/tmp", None).unwrap();
        store
            .append_turn(
                "one",
                &[
                    ConversationMessage::Chat(ChatMessage::assistant("one")),
                    ConversationMessage::Chat(ChatMessage::assistant("older")),
                ],
            )
            .unwrap();
        let page = store.load_message_page("one", 1, None).unwrap();
        let token = page.next_cursor;
        assert!(token.is_some());
        let valid_state = decode_cursor(token.as_deref().unwrap()).unwrap();
        let zero_offset = encode_cursor(AcpSessionCursor {
            next_entry_offset: Some(0),
            ..valid_state
        })
        .unwrap();
        let valid_state = decode_cursor(token.as_deref().unwrap()).unwrap();
        let terminal_position = encode_cursor(AcpSessionCursor {
            next_message_id: 0,
            next_entry_offset: None,
            ..valid_state
        })
        .unwrap();
        assert!(store.load_message_page("one", 1, Some("nope")).is_err());
        assert!(store.load_message_page("two", 1, token.as_deref()).is_err());
        assert!(
            store
                .load_message_page("one", 1, Some(&zero_offset))
                .is_err()
        );
        assert!(
            store
                .load_message_page("one", 1, Some(&terminal_position))
                .is_err()
        );
        assert!(store.load_message_page("one", 1, Some("acp1.éé")).is_err());
        assert!(
            store
                .load_message_page("one", 1, Some("acp1.7b7d226e76657273696f6e223a327d"))
                .is_err()
        );
        assert!(store.load_message_page("one", 0, None).is_err());
        assert!(store.load_message_page("one", 1_001, None).is_err());
        assert!(store.load_message_page("one", 1_000, None).is_ok());
    }

    #[test]
    fn oversized_group_materializes_only_requested_projected_entries() {
        let (_tmp, store) = open_store();
        store
            .create_session("large", "alpha", "/tmp", None)
            .unwrap();
        let calls = (0..128)
            .map(|index| ToolCall {
                id: format!("call-{index}"),
                name: "shell".into(),
                arguments: format!("{{\"index\":{index}}}"),
                extra_content: None,
            })
            .collect::<Vec<_>>();
        store
            .append_turn(
                "large",
                &[ConversationMessage::AssistantToolCalls {
                    text: None,
                    tool_calls: calls,
                    reasoning_content: None,
                }],
            )
            .unwrap();

        let page = store.load_message_page("large", 1, None).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert!(matches!(
            &page.messages[0],
            ConversationMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls[0].id == "call-127"
        ));
        let older = store
            .load_message_page("large", 1, page.next_cursor.as_deref())
            .unwrap();
        assert_eq!(older.messages.len(), 1);
        assert!(matches!(
            &older.messages[0],
            ConversationMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls[0].id == "call-126"
        ));
    }

    #[test]
    fn delete_session_cascades_to_children() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-del", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .append_turn(
                "sess-del",
                &[
                    ConversationMessage::AssistantToolCalls {
                        text: Some("calling".into()),
                        tool_calls: vec![ToolCall {
                            id: "tc-1".into(),
                            name: "shell".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        }],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![ToolResultMessage {
                        tool_call_id: "tc-1".into(),
                        content: "ok".into(),
                        tool_name: String::new(),
                    }]),
                ],
            )
            .unwrap();
        store
            .append_event("sess-del", Action::Disconnect, EventOutcome::Success, None)
            .unwrap();

        assert!(store.delete_session("sess-del").unwrap());

        let conn = store.conn.lock();
        for table in ["acp_messages", "acp_tool_calls", "acp_session_events"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "cascade should empty {table}");
        }
    }

    #[test]
    fn delete_nonexistent_session_returns_false() {
        let (_tmp, store) = open_store();
        assert!(!store.delete_session("ghost").unwrap());
    }

    #[test]
    fn owned_delete_is_an_atomic_ownership_predicate() {
        let (_tmp, store) = open_store();
        store
            .create_session("owned", "agent", "/ws", Some("user:alice"))
            .unwrap();

        assert!(
            !store.delete_session_owned("owned", "user:bob").unwrap(),
            "the wrong owner deletes nothing"
        );
        assert!(store.load_session("owned").unwrap().is_some());

        assert!(store.delete_session_owned("owned", "user:alice").unwrap());
        assert!(store.load_session("owned").unwrap().is_none());
    }

    #[test]
    fn owned_delete_refuses_null_owner_rows() {
        let (_tmp, store) = open_store();
        store
            .create_session("legacy", "agent", "/ws", None)
            .unwrap();
        assert!(
            !store.delete_session_owned("legacy", "user:alice").unwrap(),
            "NULL never equals a principal id"
        );
        assert!(store.load_session("legacy").unwrap().is_some());
    }

    #[test]
    fn checkpoint_recovery_and_finalization_keep_the_durable_owner() {
        let (_tmp, store) = open_store();
        store
            .create_session("owned-checkpoint", "agent", "/ws", Some("user:alice"))
            .unwrap();
        let pending = ConversationMessage::Chat(ChatMessage::user("pending request"));
        store
            .begin_turn_checkpoint("owned-checkpoint", "turn-1", std::slice::from_ref(&pending))
            .unwrap();
        assert!(
            store
                .finalize_turn_checkpoint_with_context_for_owner(
                    "owned-checkpoint",
                    "turn-1",
                    &[],
                    &[],
                    false,
                    Some("user:bob"),
                )
                .is_err()
        );
        assert!(
            !store
                .recover_turn_checkpoint_for_owner(
                    "owned-checkpoint",
                    "stream interrupted",
                    Some("user:bob"),
                )
                .unwrap()
        );
        assert!(
            !store
                .delete_session_owned("owned-checkpoint", "user:bob")
                .unwrap()
        );
        assert!(
            store
                .recover_turn_checkpoint_for_owner(
                    "owned-checkpoint",
                    "stream interrupted",
                    Some("user:alice"),
                )
                .unwrap()
        );
        let restored = store.load_session("owned-checkpoint").unwrap().unwrap();
        assert_eq!(restored.principal_id.as_deref(), Some("user:alice"));
        assert!(matches!(
            &restored.messages[..],
            [ConversationMessage::Chat(user), ConversationMessage::Chat(marker)]
                if user.content == "pending request" && marker.content == "stream interrupted"
        ));
    }

    #[test]
    fn mark_session_killed_persists_without_deleting_history() {
        let (tmp, store) = open_store();
        store
            .create_session("sess-kill", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .append_turn(
                "sess-kill",
                &[ConversationMessage::Chat(ChatMessage::user("keep this"))],
            )
            .unwrap();

        assert!(!store.is_session_killed("sess-kill").unwrap());
        assert!(store.mark_session_killed("sess-kill").unwrap());
        assert!(store.is_session_killed("sess-kill").unwrap());

        let data = store.load_session("sess-kill").unwrap().unwrap();
        assert_eq!(
            data.messages.len(),
            1,
            "kill marker must not delete durable transcript history"
        );

        drop(store);
        let reopened = AcpSessionStore::new(tmp.path()).unwrap();
        assert!(
            reopened.is_session_killed("sess-kill").unwrap(),
            "kill marker must survive store reopen"
        );
        assert!(
            reopened.load_session("sess-kill").unwrap().is_some(),
            "durable history remains loadable after reopen"
        );
    }

    #[test]
    fn mark_nonexistent_session_killed_returns_false() {
        let (_tmp, store) = open_store();
        assert!(!store.mark_session_killed("ghost").unwrap());
        assert!(!store.is_session_killed("ghost").unwrap());
    }

    #[test]
    fn atomic_kill_transition_distinguishes_marked_already_killed_and_missing() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-atomic-kill", "alpha", "/tmp/proj", None)
            .unwrap();

        assert_eq!(
            store
                .mark_session_killed_atomic("sess-atomic-kill")
                .unwrap(),
            AcpSessionKillTransition::Marked
        );
        assert_eq!(
            store
                .mark_session_killed_atomic("sess-atomic-kill")
                .unwrap(),
            AcpSessionKillTransition::AlreadyKilled
        );
        assert!(
            store.mark_session_killed("sess-atomic-kill").unwrap(),
            "the compatibility wrapper must retain its existing-row contract"
        );
        assert_eq!(
            store.mark_session_killed_atomic("ghost").unwrap(),
            AcpSessionKillTransition::Missing
        );
        assert!(store.load_session("sess-atomic-kill").unwrap().is_some());
    }

    #[test]
    fn touch_session_updates_last_activity() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-touch", "alpha", "/tmp/proj", None)
            .unwrap();
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

    #[test]
    fn set_token_count_persists_and_load_reads_it() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-tok", "alpha", "/tmp/proj", None)
            .unwrap();
        assert_eq!(
            store.load_session("sess-tok").unwrap().unwrap().token_count,
            0
        );

        store.set_token_count("sess-tok", 152_306).unwrap();
        assert_eq!(
            store.load_session("sess-tok").unwrap().unwrap().token_count,
            152_306,
            "ctx-bar value must round-trip through the store"
        );

        // Overwrite semantics (not cumulative).
        store.set_token_count("sess-tok", 42).unwrap();
        assert_eq!(
            store.load_session("sess-tok").unwrap().unwrap().token_count,
            42
        );
    }

    #[test]
    fn set_token_count_errors_on_unknown_session() {
        // Defensive: a silent zero-row UPDATE would mask a race where the
        // session was deleted while a Usage event was in flight. The caller
        // needs the error so the failure is loggable.
        let (_tmp, store) = open_store();
        let err = store.set_token_count("nonexistent", 100).unwrap_err();
        assert!(
            err.to_string().contains("nonexistent"),
            "error must name the missing session_uuid; got: {err}"
        );
    }

    #[test]
    fn clear_token_count_resets_snapshot_to_unknown() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-clr", "alpha", "/tmp/proj", None)
            .unwrap();
        store.set_token_count("sess-clr", 152_306).unwrap();
        store.clear_token_count("sess-clr").unwrap();
        assert_eq!(
            store.load_session("sess-clr").unwrap().unwrap().token_count,
            0,
            "accepted usage-less call must clear the durable snapshot"
        );
    }

    #[test]
    fn clear_token_count_errors_on_unknown_session() {
        let (_tmp, store) = open_store();
        let err = store.clear_token_count("nonexistent").unwrap_err();
        assert!(
            err.to_string().contains("nonexistent"),
            "error must name the missing session_uuid; got: {err}"
        );
    }

    #[test]
    fn persist_usage_snapshot_accepted_sequence_clears_on_missing() {
        // Accepted A with usage, then accepted B without: the durable
        // snapshot must clear, not retain A's count.
        let (_tmp, store) = open_store();
        store
            .create_session("sess-seq", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .persist_usage_snapshot("sess-seq", Some(1000), true)
            .unwrap();
        assert_eq!(
            store.load_session("sess-seq").unwrap().unwrap().token_count,
            1000
        );
        store
            .persist_usage_snapshot("sess-seq", None, true)
            .unwrap();
        assert_eq!(
            store.load_session("sess-seq").unwrap().unwrap().token_count,
            0,
            "accepted usage-less call must clear the durable snapshot"
        );
    }

    #[test]
    fn persist_usage_snapshot_rejected_never_touches_store() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-rej", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .persist_usage_snapshot("sess-rej", Some(1000), true)
            .unwrap();
        store
            .persist_usage_snapshot("sess-rej", Some(5000), false)
            .unwrap();
        store
            .persist_usage_snapshot("sess-rej", None, false)
            .unwrap();
        assert_eq!(
            store.load_session("sess-rej").unwrap().unwrap().token_count,
            1000,
            "rejected billing telemetry is billing-only"
        );
    }

    #[test]
    fn append_event_writes_action_outcome_payload() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-evt", "alpha", "/tmp/proj", None)
            .unwrap();

        store
            .append_event(
                "sess-evt",
                Action::Cancel,
                EventOutcome::Failure,
                Some("turn cancelled by user"),
            )
            .unwrap();

        let conn = store.conn.lock();
        let (action, outcome, payload): (String, String, Option<String>) = conn
            .query_row(
                "SELECT action, outcome, payload FROM acp_session_events LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(action, "cancel");
        assert_eq!(outcome, "failure");
        assert_eq!(payload.as_deref(), Some("turn cancelled by user"));
    }

    #[test]
    fn list_sessions_returns_summaries_ordered_by_recent_activity() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-old", "alpha", "/tmp/a", None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .create_session("sess-new", "beta", "/tmp/b", None)
            .unwrap();
        store
            .append_turn(
                "sess-new",
                &[ConversationMessage::Chat(ChatMessage::user("hi"))],
            )
            .unwrap();
        store.set_token_count("sess-new", 1234).unwrap();

        let list = store.list_sessions().unwrap();
        assert_eq!(list.len(), 2);
        // Most recent activity first.
        assert_eq!(list[0].session_uuid, "sess-new");
        assert_eq!(list[0].agent_alias, "beta");
        assert_eq!(list[0].workspace_dir, "/tmp/b");
        assert_eq!(list[0].message_count, 1);
        assert_eq!(list[0].token_count, 1234);
        assert_eq!(list[1].session_uuid, "sess-old");
        assert_eq!(list[1].message_count, 0);
    }

    #[test]
    fn list_sessions_empty_when_no_sessions() {
        let (_tmp, store) = open_store();
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn list_sessions_omits_killed_sessions() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-live", "alpha", "/tmp/live", None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .create_session("sess-killed", "alpha", "/tmp/killed", None)
            .unwrap();
        store.mark_session_killed("sess-killed").unwrap();

        let list = store.list_sessions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_uuid, "sess-live");
    }

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: "shell".into(),
            arguments: "{}".into(),
            extra_content: None,
        }
    }

    fn tool_result(id: &str) -> ToolResultMessage {
        ToolResultMessage {
            tool_call_id: id.to_string(),
            content: "out".into(),
            tool_name: "shell".into(),
        }
    }

    fn assert_list_matches_projection(
        store: &AcpSessionStore,
        session_uuid: &str,
        expected: usize,
    ) {
        let summary = &store
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|s| s.session_uuid == session_uuid)
            .unwrap_or_else(|| panic!("session {session_uuid} should be listed"));
        let data = store.load_session(session_uuid).unwrap().unwrap();
        assert_eq!(summary.message_count, expected, "session {session_uuid}");
        assert_eq!(
            summary.message_count,
            projected_entry_count(&data.messages),
            "session {session_uuid} persisted count must match the projection"
        );
        // The live agent-scoped listing must agree with the other listings:
        // it reads the same persisted counter, never a fresh row count.
        let live = &store
            .list_live_sessions_by_agent(&summary.agent_alias)
            .unwrap()
            .into_iter()
            .find(|s| s.session_uuid == session_uuid)
            .unwrap_or_else(|| {
                panic!("session {session_uuid} should be live-listed for its agent")
            });
        assert_eq!(
            live.message_count, expected,
            "session {session_uuid} live listing must match the projection"
        );
    }

    #[test]
    fn list_sessions_message_count_matches_projection_after_mixed_writes() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-mixed", "alpha", "/tmp/mixed", None)
            .unwrap();

        // Chat only.
        store
            .append_turn(
                "sess-mixed",
                &[ConversationMessage::Chat(ChatMessage::user("hi"))],
            )
            .unwrap();
        assert_list_matches_projection(&store, "sess-mixed", 1);

        // One batch of 3 calls with text: text + 3 calls = +4. Results come
        // in a later turn so the cross-turn fold is exercised there.
        store
            .append_turn(
                "sess-mixed",
                &[ConversationMessage::AssistantToolCalls {
                    text: Some("inspecting".into()),
                    tool_calls: vec![tool_call("a"), tool_call("b"), tool_call("c")],
                    reasoning_content: None,
                }],
            )
            .unwrap();
        assert_list_matches_projection(&store, "sess-mixed", 5);

        // One batch of 2 calls with empty text: calls only = +2.
        store
            .append_turn(
                "sess-mixed",
                &[ConversationMessage::AssistantToolCalls {
                    text: Some(String::new()),
                    tool_calls: vec![tool_call("d"), tool_call("e")],
                    reasoning_content: None,
                }],
            )
            .unwrap();
        assert_list_matches_projection(&store, "sess-mixed", 7);

        // Results folding into the same-turn call ("f") and into calls
        // written in PREVIOUS append_turns ("a", "b", "c"): only the new
        // call adds an entry.
        store
            .append_turn(
                "sess-mixed",
                &[
                    ConversationMessage::AssistantToolCalls {
                        text: None,
                        tool_calls: vec![tool_call("f")],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![
                        tool_result("f"),
                        tool_result("a"),
                        tool_result("b"),
                        tool_result("c"),
                    ]),
                ],
            )
            .unwrap();
        assert_list_matches_projection(&store, "sess-mixed", 8);

        // An orphan result (no call was ever written for it) counts as its
        // own entry on top of the same-turn call; the unconsumed previous
        // turn's call ("d") still folds.
        store
            .append_turn(
                "sess-mixed",
                &[
                    ConversationMessage::AssistantToolCalls {
                        text: None,
                        tool_calls: vec![tool_call("g")],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![
                        tool_result("g"),
                        tool_result("d"),
                        tool_result("orphan"),
                    ]),
                ],
            )
            .unwrap();
        assert_list_matches_projection(&store, "sess-mixed", 10);
    }

    #[test]
    fn append_turn_orphan_result_does_not_consume_later_reused_call() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-reuse", "alpha", "/tmp/reuse", None)
            .unwrap();

        // Turn 1 issues x. Turn 2 must open a batch before its results, so y
        // rides along and never resolves; its second x result has no call
        // left to fold into. Turn 3 reuses the x id.
        store
            .append_turn(
                "sess-reuse",
                &[ConversationMessage::AssistantToolCalls {
                    text: None,
                    tool_calls: vec![tool_call("x")],
                    reasoning_content: None,
                }],
            )
            .unwrap();
        store
            .append_turn(
                "sess-reuse",
                &[
                    ConversationMessage::AssistantToolCalls {
                        text: None,
                        tool_calls: vec![tool_call("y")],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![tool_result("x"), tool_result("x")]),
                ],
            )
            .unwrap();
        store
            .append_turn(
                "sess-reuse",
                &[
                    ConversationMessage::AssistantToolCalls {
                        text: None,
                        tool_calls: vec![tool_call("x")],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![tool_result("x")]),
                ],
            )
            .unwrap();

        // Entries: call x, call y, folded result, orphan duplicate result,
        // reused call x, folded result. The orphan duplicate must leave the
        // open-call balance untouched so the reused call's result folds.
        assert_list_matches_projection(&store, "sess-reuse", 4);
    }

    #[test]
    fn open_calls_after_clamps_orphan_results_at_zero() {
        assert_eq!(open_calls_after([ToolEventKind::In]), 1);
        assert_eq!(open_calls_after([ToolEventKind::In, ToolEventKind::Out]), 0);
        // A duplicate out is an orphan entry and leaves the balance at zero.
        assert_eq!(
            open_calls_after([ToolEventKind::In, ToolEventKind::Out, ToolEventKind::Out]),
            0
        );
        // So a later call reusing the id still opens fresh.
        assert_eq!(
            open_calls_after([
                ToolEventKind::In,
                ToolEventKind::Out,
                ToolEventKind::Out,
                ToolEventKind::In,
            ]),
            1
        );
        assert_eq!(open_calls_after([ToolEventKind::Out]), 0);
        assert_eq!(
            open_calls_after([ToolEventKind::In, ToolEventKind::In, ToolEventKind::Out]),
            1
        );
    }

    #[test]
    fn append_turn_orphan_result_after_callless_assistant_chat_keeps_parent() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-callless-chat", "alpha", "/tmp/callless-chat", None)
            .unwrap();

        // An orphan result attaches to the most recent assistant message,
        // which may be a plain chat with no calls in the turn. The persisted
        // counter scores the parent text and the orphan result as separate
        // entries, so the reload must keep both.
        store
            .append_turn(
                "sess-callless-chat",
                &[
                    ConversationMessage::Chat(ChatMessage::user("q")),
                    ConversationMessage::Chat(ChatMessage::assistant("a")),
                    ConversationMessage::ToolResults(vec![tool_result("z")]),
                ],
            )
            .unwrap();

        let list = store.list_sessions().unwrap();
        assert_eq!(list[0].message_count, 3);
        let data = store.load_session("sess-callless-chat").unwrap().unwrap();
        assert_eq!(list[0].message_count, projected_entry_count(&data.messages));
        assert!(matches!(
            &data.messages[1],
            ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "a"
        ));
        match &data.messages[2] {
            ConversationMessage::ToolResults(results) => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].tool_call_id, "z");
            }
            other => panic!("expected ToolResults, got {other:?}"),
        }
    }

    #[test]
    fn append_turn_orphan_result_after_callless_batch_keeps_parent_text() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-callless-batch", "alpha", "/tmp/callless-batch", None)
            .unwrap();

        // A text-only batch (no calls) persists its text as an assistant
        // row and sets the parent for later results, so an orphan result
        // riding the same turn must not drop that text on reload.
        store
            .append_turn(
                "sess-callless-batch",
                &[
                    ConversationMessage::Chat(ChatMessage::user("q")),
                    ConversationMessage::AssistantToolCalls {
                        text: Some("t".into()),
                        tool_calls: vec![],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![tool_result("z")]),
                ],
            )
            .unwrap();

        let list = store.list_sessions().unwrap();
        assert_eq!(list[0].message_count, 3);
        let data = store.load_session("sess-callless-batch").unwrap().unwrap();
        assert_eq!(list[0].message_count, projected_entry_count(&data.messages));
        assert!(matches!(
            &data.messages[1],
            ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "t"
        ));
        assert!(matches!(
            &data.messages[2],
            ConversationMessage::ToolResults(results)
                if results.len() == 1 && results[0].tool_call_id == "z"
        ));
    }

    /// The schema as it was before the projected-count columns existed.
    /// `AcpSessionStore::new` adds the rest of the current columns through
    /// its idempotent migrations, so tests only hand-build what predates
    /// the change under test.
    fn legacy_schema_without_projected_count(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE acp_sessions (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_uuid  TEXT NOT NULL UNIQUE,
                 agent_alias   TEXT NOT NULL,
                 workspace_dir TEXT NOT NULL,
                 interaction_surface TEXT,
                 token_count   INTEGER NOT NULL DEFAULT 0,
                 killed_at     TEXT,
                 created_at    TEXT NOT NULL,
                 last_activity TEXT NOT NULL
             );
             CREATE TABLE acp_messages (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id  INTEGER NOT NULL REFERENCES acp_sessions(id) ON DELETE CASCADE,
                 role        TEXT NOT NULL,
                 content     TEXT NOT NULL,
                 reasoning_content TEXT,
                 created_at  TEXT NOT NULL
             );
             CREATE TABLE acp_tool_calls (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 message_id   INTEGER NOT NULL REFERENCES acp_messages(id) ON DELETE CASCADE,
                 tool_call_id TEXT NOT NULL,
                 tool_name    TEXT NOT NULL,
                 event_kind   TEXT NOT NULL,
                 payload      TEXT NOT NULL,
                 outcome      TEXT,
                 created_at   TEXT NOT NULL
             );",
        )
        .unwrap();
    }

    /// Read the raw cache pair, bypassing the read paths, so a test can tell
    /// a session that was never scored (0, NULL) apart from one the store
    /// scored (count, watermark) and from a cache another writer left
    /// behind the rows.
    fn raw_count_pair(store: &AcpSessionStore, session_uuid: &str) -> (i64, Option<i64>) {
        store
            .conn
            .lock()
            .query_row(
                "SELECT projected_message_count, projected_count_through
                  FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn rollback_writes_by_an_older_binary_self_heal_on_the_next_list() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store
            .create_session("self-heal-s", "alpha", "/tmp/s", None)
            .unwrap();
        store
            .append_turn(
                "self-heal-s",
                &[ConversationMessage::Chat(ChatMessage::user("first"))],
            )
            .unwrap();
        assert_list_matches_projection(&store, "self-heal-s", 1);
        drop(store);

        // A binary without the cache columns writes the same file: it
        // inserts session T and appends rows to S without touching either
        // column, so both caches are left behind the rows.
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('old-binary-t', 'alpha', '/tmp/t', 0, 't', 't')",
            [],
        )
        .unwrap();
        let s_id: i64 = conn
            .query_row(
                "SELECT id FROM acp_sessions WHERE session_uuid = 'self-heal-s'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let t_id: i64 = conn
            .query_row(
                "SELECT id FROM acp_sessions WHERE session_uuid = 'old-binary-t'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (?1, 'user', 'old binary q', 't')",
            params![t_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (?1, 'assistant', 'old binary a', 't')",
            params![t_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (?1, 'user', 'old binary turn', 't')",
            params![s_id],
        )
        .unwrap();
        drop(conn);

        // The next list self-heals both sessions from their rows.
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert_list_matches_projection(&store, "self-heal-s", 2);
        assert_list_matches_projection(&store, "old-binary-t", 2);

        // A later append then increments the healed base, never a stale one.
        store
            .append_turn(
                "self-heal-s",
                &[ConversationMessage::Chat(ChatMessage::user(
                    "after upgrade",
                ))],
            )
            .unwrap();
        assert_list_matches_projection(&store, "self-heal-s", 3);
        assert_eq!(
            store.projected_message_count("self-heal-s").unwrap(),
            Some(3)
        );
    }

    #[test]
    fn old_binary_emptying_a_transcript_reads_as_zero() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        store
            .create_session("emptied-s", "alpha", "/tmp/e", None)
            .unwrap();
        store
            .append_turn(
                "emptied-s",
                &[
                    ConversationMessage::Chat(ChatMessage::user("q")),
                    ConversationMessage::Chat(ChatMessage::assistant("a")),
                ],
            )
            .unwrap();
        assert_list_matches_projection(&store, "emptied-s", 2);
        drop(store);

        // A binary without the cache columns empties the transcript without
        // knowing either column, leaving the pair behind rows that no longer
        // exist.
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "DELETE FROM acp_messages WHERE session_id =
                 (SELECT id FROM acp_sessions WHERE session_uuid = 'emptied-s')",
            [],
        )
        .unwrap();
        drop(conn);

        // The next read heals to the empty transcript: zero rows, zero
        // count, NULL watermark (valid at 0).
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        let list = store.list_sessions().unwrap();
        let emptied = list.iter().find(|s| s.session_uuid == "emptied-s").unwrap();
        assert_eq!(emptied.message_count, 0);
        assert_eq!(store.projected_message_count("emptied-s").unwrap(), Some(0));
        assert_eq!(raw_count_pair(&store, "emptied-s"), (0, None));
    }

    #[test]
    fn unreadable_session_does_not_block_the_store_or_other_sessions() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        let conn = Connection::open(&db_path).unwrap();
        legacy_schema_without_projected_count(&conn);

        // Session A (healthy): a chat, a batch with text and two calls,
        // both results folding: 4 entries.
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('healthy-a', 'alpha', '/tmp/a', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'user', 'hi', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'assistant', 'working', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (2, 'x', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (2, 'y', 'read', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (2, 'x', 'shell', 'out', '/tmp', 'unknown', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (2, 'y', 'read', 'out', 'data', 'unknown', 't')",
            [],
        )
        .unwrap();

        // Session B: an assistant text and one call, plus one row whose
        // event_kind no reload accepts.
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('unreadable-b', 'alpha', '/tmp/b', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (2, 'assistant', 'plan', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (3, 'z', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (3, 'z', 'shell', 'bogus', 'lost', 'unknown', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        let store = AcpSessionStore::new(tmp.path())
            .expect("an unreadable session must not fail the store open");
        let list = store.list_sessions().unwrap();
        assert_eq!(list.len(), 2, "both sessions must list");
        assert_list_matches_projection(&store, "healthy-a", 4);
        let unreadable = list
            .iter()
            .find(|s| s.session_uuid == "unreadable-b")
            .unwrap();
        assert_eq!(
            unreadable.message_count, 0,
            "a session that was never scored lists with its stored count"
        );
        assert!(
            store.load_session("unreadable-b").is_err(),
            "the unreadable session must still fail closed on load"
        );

        // Repairing the row lets the next list score the session.
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE acp_tool_calls SET event_kind = 'out' WHERE event_kind = 'bogus'",
            [],
        )
        .unwrap();
        drop(conn);
        assert_list_matches_projection(&store, "unreadable-b", 2);
    }

    #[test]
    fn lazy_refresh_scores_pre_column_rows_on_first_list() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();

        // A pre-column database: the helper builds the schema as it was,
        // and the open adds the rest of the current columns.
        let conn = Connection::open(&db_path).unwrap();
        legacy_schema_without_projected_count(&conn);

        // Session 1: chat (1), batch with text + 2 calls (3), results fold.
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('old-full', 'alpha', '/tmp/a', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'user', 'hi', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'assistant', 'working', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (2, 'x', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (2, 'y', 'read', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (2, 'x', 'shell', 'out', '/tmp', 'unknown', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (2, 'y', 'read', 'out', 'data', 'unknown', 't')",
            [],
        )
        .unwrap();

        // Session 2: empty-text batch (0 text entries), one call whose
        // result folds, and one orphan 'out' row for a call never issued.
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('old-orphan', 'alpha', '/tmp/b', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (2, 'assistant', '', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (3, 'z', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (3, 'z', 'shell', 'out', 'ok', 'unknown', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (3, 'ghost', 'shell', 'out', 'lost', 'unknown', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        // Opening the store adds the cache columns without scoring anyone:
        // the pre-column rows stay at the column default with a NULL
        // watermark until a read scores them.
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert_eq!(raw_count_pair(&store, "old-full"), (0, None));
        assert_eq!(raw_count_pair(&store, "old-orphan"), (0, None));

        // The first list scores both sessions from their rows and moves
        // each watermark to the session's own highest row id.
        assert_list_matches_projection(&store, "old-full", 4);
        assert_list_matches_projection(&store, "old-orphan", 2);
        assert_eq!(raw_count_pair(&store, "old-full"), (4, Some(2)));
        assert_eq!(raw_count_pair(&store, "old-orphan"), (2, Some(3)));

        // A post-scoring append keeps incrementing from the scored base.
        store
            .append_turn(
                "old-full",
                &[ConversationMessage::Chat(ChatMessage::user("again"))],
            )
            .unwrap();
        assert_list_matches_projection(&store, "old-full", 5);

        // A valid cache is not rescored: rewriting the count by raw SQL
        // without moving the watermark pins the refresh to the watermark
        // and not to the list itself.
        {
            let conn = store.conn.lock();
            conn.execute(
                "UPDATE acp_sessions SET projected_message_count = 99 WHERE session_uuid = 'old-full'",
                [],
            )
            .unwrap();
        }
        let list = store.list_sessions().unwrap();
        let full = list.iter().find(|s| s.session_uuid == "old-full").unwrap();
        assert_eq!(
            full.message_count, 99,
            "a valid cache must survive a list without rescore"
        );
        let orphan = list
            .iter()
            .find(|s| s.session_uuid == "old-orphan")
            .unwrap();
        assert_eq!(orphan.message_count, 2);
    }

    #[test]
    fn lazy_refresh_partitions_tool_call_pairing_by_session() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();

        // A pre-column database: the helper builds the schema as it was,
        // and the open adds the rest of the current columns.
        let conn = Connection::open(&db_path).unwrap();
        legacy_schema_without_projected_count(&conn);

        // Session A (id 1): an unmatched 'in' row for tool-call ID 'x'.
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('session-a', 'alpha', '/tmp/a', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'assistant', '', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (1, 'x', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();

        // Session B (id 2): 'in y' followed by an orphan 'out x'. Tool-call
        // IDs are scoped per session, so B's 'out x' must NOT consume A's
        // unmatched 'in x'.
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('session-b', 'alpha', '/tmp/b', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (2, 'assistant', '', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (2, 'y', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (2, 'x', 'shell', 'out', 'lost', 'unknown', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        // The open adds the cache columns without scoring anyone.
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert_eq!(raw_count_pair(&store, "session-a"), (0, None));
        assert_eq!(raw_count_pair(&store, "session-b"), (0, None));

        // The first list scores both: A: 1 entry for the unmatched 'in x'.
        // B: 1 for 'in y' plus 1 for the orphan 'out x' (2 total); a
        // cross-session pairing would fold B's orphan into A's call and
        // undercount B to 1.
        assert_list_matches_projection(&store, "session-a", 1);
        assert_list_matches_projection(&store, "session-b", 2);
    }

    #[test]
    fn lazy_refresh_orphan_result_does_not_consume_later_reused_call() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();

        // A pre-column database: the helper builds the schema as it was,
        // and the open adds the rest of the current columns.
        let conn = Connection::open(&db_path).unwrap();
        legacy_schema_without_projected_count(&conn);

        // One session, one id: the call, its result, a duplicate result with
        // no call left to fold into, then a reused call and its result.
        conn.execute(
            "INSERT INTO acp_sessions
                (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('old-reuse', 'alpha', '/tmp/r', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'assistant', '', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'assistant', '', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (1, 'x', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (1, 'x', 'shell', 'out', 'ok', 'unknown', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (1, 'x', 'shell', 'out', 'duplicate', 'unknown', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, created_at)
             VALUES (2, 'x', 'shell', 'in', '{}', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (2, 'x', 'shell', 'out', 'again', 'unknown', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        // Entries: call, fold, orphan duplicate, reused call, fold = 3. The
        // orphan duplicate must not consume the reused call. The open adds
        // the cache columns without scoring anyone; the first list scores.
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert_eq!(raw_count_pair(&store, "old-reuse"), (0, None));
        assert_list_matches_projection(&store, "old-reuse", 3);
    }

    #[test]
    fn lazy_refresh_keeps_callless_parent_with_orphan_result() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions").join("acp-sessions.db");
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();

        // A pre-column database: the helper builds the schema as it was,
        // and the open adds the rest of the current columns.
        let conn = Connection::open(&db_path).unwrap();
        legacy_schema_without_projected_count(&conn);

        // One session: a user chat, an assistant row with text and no
        // calls, and an orphan result attached to that assistant row.
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES ('old-callless', 'alpha', '/tmp/c', 0, 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'user', 'q', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_messages (session_id, role, content, created_at)
             VALUES (1, 'assistant', 'a', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO acp_tool_calls (message_id, tool_call_id, tool_name, event_kind, payload, outcome, created_at)
             VALUES (2, 'z', 'shell', 'out', 'lost', 'unknown', 't')",
            [],
        )
        .unwrap();
        drop(conn);

        // The open adds the cache columns without scoring anyone. The
        // refresh scores the reloaded projection, so the callless parent
        // text and the orphan result must both reload for the scored count
        // to see all three entries.
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        assert_eq!(raw_count_pair(&store, "old-callless"), (0, None));
        assert_list_matches_projection(&store, "old-callless", 3);
        let data = store.load_session("old-callless").unwrap().unwrap();
        assert!(matches!(
            &data.messages[1],
            ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "a"
        ));
    }

    #[test]
    fn list_live_sessions_by_agent_filters_owner_and_killed_rows() {
        let (_tmp, store) = open_store();
        store
            .create_session("alpha-old", "alpha", "/ws/old", None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .create_session("alpha-new", "alpha", "/ws/new", None)
            .unwrap();
        store
            .append_turn(
                "alpha-new",
                &[ConversationMessage::Chat(ChatMessage::user("hello"))],
            )
            .unwrap();
        store
            .create_session("alpha-killed", "alpha", "/ws/killed", None)
            .unwrap();
        store.mark_session_killed("alpha-killed").unwrap();
        store
            .create_session("beta-live", "beta", "/ws/beta", None)
            .unwrap();

        let list = store.list_live_sessions_by_agent("alpha").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_uuid, "alpha-new");
        assert_eq!(list[0].message_count, 1);
        assert_eq!(list[1].session_uuid, "alpha-old");
        assert!(list.iter().all(|summary| summary.agent_alias == "alpha"));
        assert!(
            store
                .list_live_sessions_by_agent("missing")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn load_session_for_agent_authorizes_uuid_and_preserves_projection() {
        let (_tmp, store) = open_store();
        store
            .create_session_with_interaction_surface(
                "owned",
                "alpha",
                "/ws/alpha",
                Some("zerocode_code"),
                None,
            )
            .unwrap();
        store
            .append_turn(
                "owned",
                &[
                    ConversationMessage::Chat(ChatMessage::user("hello")),
                    ConversationMessage::AssistantToolCalls {
                        text: Some("calling".into()),
                        tool_calls: vec![ToolCall {
                            id: "tc-1".into(),
                            name: "shell".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        }],
                        reasoning_content: Some("thinking".into()),
                    },
                    ConversationMessage::ToolResults(vec![ToolResultMessage {
                        tool_call_id: "tc-1".into(),
                        content: "done".into(),
                        tool_name: String::new(),
                    }]),
                ],
            )
            .unwrap();

        let data = store
            .load_session_for_agent("owned", "alpha")
            .unwrap()
            .expect("matching owner should load");
        assert_eq!(data.agent_alias, "alpha");
        assert_eq!(data.interaction_surface.as_deref(), Some("zerocode_code"));
        assert_eq!(data.messages.len(), 3);
        assert!(matches!(
            &data.messages[0],
            ConversationMessage::Chat(message)
                if message.role == "user" && message.content == "hello"
        ));
        assert!(matches!(
            &data.messages[1],
            ConversationMessage::AssistantToolCalls {
                text: Some(text),
                tool_calls,
                reasoning_content: Some(reasoning),
            }
                if text == "calling"
                    && tool_calls.len() == 1
                    && tool_calls[0].id == "tc-1"
                    && reasoning == "thinking"
        ));
        assert!(matches!(
            &data.messages[2],
            ConversationMessage::ToolResults(results)
                if results.len() == 1
                    && results[0].tool_call_id == "tc-1"
                    && results[0].content == "done"
        ));

        // SQL-level authorization deliberately gives the same result for a
        // foreign owner and a UUID that does not exist.
        assert!(
            store
                .load_session_for_agent("owned", "beta")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .load_session_for_agent("unknown", "alpha")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.classify_session_for_agent("owned", "alpha").unwrap(),
            AcpSessionAccess::Owned
        );
        assert_eq!(
            store.classify_session_for_agent("owned", "beta").unwrap(),
            AcpSessionAccess::Foreign
        );
        assert_eq!(
            store
                .classify_session_for_agent("unknown", "alpha")
                .unwrap(),
            AcpSessionAccess::Missing
        );
        assert!(store.is_live_session_for_agent("owned", "alpha").unwrap());
        assert_eq!(store.list_session_ids().unwrap(), vec!["owned"]);
    }

    #[test]
    fn per_agent_cascade_counts_live_and_deletes_only_that_agent() {
        let (_tmp, store) = open_store();
        store
            .create_session("a-live", "alpha", "/ws/a1", None)
            .unwrap();
        store
            .create_session("a-killed", "alpha", "/ws/a2", None)
            .unwrap();
        store.mark_session_killed("a-killed").unwrap();
        store
            .create_session("b-live", "beta", "/ws/b1", None)
            .unwrap();

        // Only un-killed sessions count as live (the HARD-refuse signal).
        assert_eq!(store.count_live_sessions_by_agent("alpha").unwrap(), 1);
        assert_eq!(store.count_live_sessions_by_agent("beta").unwrap(), 1);
        assert_eq!(store.count_live_sessions_by_agent("ghost").unwrap(), 0);

        // list_by_agent returns all (live + killed) for export.
        assert_eq!(store.list_sessions_by_agent("alpha").unwrap().len(), 2);

        // delete_by_agent removes exactly that agent's sessions.
        assert_eq!(store.delete_sessions_by_agent("alpha").unwrap(), 2);
        assert!(store.list_sessions_by_agent("alpha").unwrap().is_empty());
        assert_eq!(store.list_sessions_by_agent("beta").unwrap().len(), 1);
    }

    #[test]
    fn rename_sessions_by_agent_repoints_live_and_killed() {
        let (_tmp, store) = open_store();
        store
            .create_session("a-live", "alpha", "/ws/a1", None)
            .unwrap();
        store
            .create_session("a-killed", "alpha", "/ws/a2", None)
            .unwrap();
        store.mark_session_killed("a-killed").unwrap();
        store
            .create_session("b-live", "beta", "/ws/b1", None)
            .unwrap();

        // Rename re-points BOTH live and killed sessions; unlike delete, a live
        // session is no obstacle.
        assert_eq!(store.rename_sessions_by_agent("alpha", "gamma").unwrap(), 2);
        assert!(store.list_sessions_by_agent("alpha").unwrap().is_empty());
        assert_eq!(store.list_sessions_by_agent("gamma").unwrap().len(), 2);
        // the live session followed the rename
        assert_eq!(store.count_live_sessions_by_agent("gamma").unwrap(), 1);
        // beta untouched
        assert_eq!(store.list_sessions_by_agent("beta").unwrap().len(), 1);
        // unknown source → 0
        assert_eq!(store.rename_sessions_by_agent("ghost", "x").unwrap(), 0);
    }

    #[test]
    fn trim_breadcrumb_provenance_is_a_canonical_column_not_inferred_from_text() {
        // Regression: restore call sites used to infer breadcrumb ownership by
        // comparing the first stored message's text against the localized
        // breadcrumb string. A genuine user turn that happens to contain that
        // exact text must NOT be misclassified as a synthetic breadcrumb, and
        // a session that never trimmed must restore with `trim_breadcrumb ==
        // false` regardless of message content.
        let (_tmp, store) = open_store();
        store
            .create_session("sess-genuine-text", "alpha", "/tmp/proj", None)
            .unwrap();
        // A real user message that happens to equal a breadcrumb-shaped string.
        store
            .append_turn(
                "sess-genuine-text",
                &[ConversationMessage::Chat(ChatMessage::user(
                    "(earlier history was trimmed)",
                ))],
            )
            .unwrap();

        let restored = match store.load_session_for_restore("sess-genuine-text").unwrap() {
            AcpSessionRestore::Restorable(data) => data,
            AcpSessionRestore::Missing => panic!("expected a restorable session, got Missing"),
            AcpSessionRestore::Killed => panic!("expected a restorable session, got Killed"),
        };
        assert!(
            !restored.trim_breadcrumb,
            "a genuine user message with breadcrumb-shaped text must not be \
             classified as a synthetic breadcrumb absent an explicit flag"
        );
    }

    #[test]
    fn legacy_null_trim_breadcrumb_is_inferred_once_then_recorded() {
        // Simulate a row written before the `trim_breadcrumb` column
        // existed: the column is `NULL`, not `0`. Restore must infer
        // provenance from the leading synthetic marker on this one-time
        // migration, not treat `NULL` the same as an explicit "no
        // breadcrumb" and drop/miscount the marker.
        let (_tmp, store) = open_store();
        store
            .create_session("sess-legacy-marker", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .append_turn(
                "sess-legacy-marker",
                &[
                    ConversationMessage::Chat(ChatMessage::user(HISTORY_TRIM_BREADCRUMB_CANONICAL)),
                    ConversationMessage::Chat(ChatMessage::user("real turn")),
                ],
            )
            .unwrap();
        // Force the column back to NULL to simulate a pre-migration row.
        store
            .conn
            .lock()
            .execute(
                "UPDATE acp_sessions SET trim_breadcrumb = NULL WHERE session_uuid = ?1",
                params!["sess-legacy-marker"],
            )
            .unwrap();

        let restored = match store
            .load_session_for_restore("sess-legacy-marker")
            .unwrap()
        {
            AcpSessionRestore::Restorable(data) => data,
            AcpSessionRestore::Missing => panic!("expected a restorable session, got Missing"),
            AcpSessionRestore::Killed => panic!("expected a restorable session, got Killed"),
        };
        assert!(
            restored.trim_breadcrumb,
            "a NULL (legacy) row with a leading canonical marker must be \
             inferred as carrying the breadcrumb"
        );
        assert_eq!(
            raw_trim_breadcrumb_column(&store, "sess-legacy-marker"),
            Some(1),
            "the one-time inference must be recorded back to the column, \
             not re-inferred from text on every restore"
        );

        // A genuine colliding user turn (no synthetic marker at all) must
        // NOT be misclassified when the column is legacy-NULL either.
        store
            .create_session("sess-legacy-no-marker", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .append_turn(
                "sess-legacy-no-marker",
                &[ConversationMessage::Chat(ChatMessage::user("hello"))],
            )
            .unwrap();
        store
            .conn
            .lock()
            .execute(
                "UPDATE acp_sessions SET trim_breadcrumb = NULL WHERE session_uuid = ?1",
                params!["sess-legacy-no-marker"],
            )
            .unwrap();
        let restored_clean = match store
            .load_session_for_restore("sess-legacy-no-marker")
            .unwrap()
        {
            AcpSessionRestore::Restorable(data) => data,
            AcpSessionRestore::Missing => panic!("expected a restorable session, got Missing"),
            AcpSessionRestore::Killed => panic!("expected a restorable session, got Killed"),
        };
        assert!(
            !restored_clean.trim_breadcrumb,
            "a legacy-NULL row with no marker-shaped text must not be inferred as true"
        );
        assert_eq!(
            raw_trim_breadcrumb_column(&store, "sess-legacy-no-marker"),
            Some(0),
            "the one-time inference must be recorded back to the column even \
             when it infers false, not left NULL to be re-inferred later"
        );
    }

    #[test]
    fn trim_breadcrumb_survives_restore_and_a_second_trim() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-trimmed", "alpha", "/tmp/proj", None)
            .unwrap();
        store
            .append_turn(
                "sess-trimmed",
                &[ConversationMessage::Chat(ChatMessage::user(
                    "most recent turn",
                ))],
            )
            .unwrap();

        // First trim: mark the session as carrying a synthetic breadcrumb.
        store.set_trim_breadcrumb("sess-trimmed", true).unwrap();
        let restored = match store.load_session_for_restore("sess-trimmed").unwrap() {
            AcpSessionRestore::Restorable(data) => data,
            AcpSessionRestore::Missing => panic!("expected a restorable session, got Missing"),
            AcpSessionRestore::Killed => panic!("expected a restorable session, got Killed"),
        };
        assert!(
            restored.trim_breadcrumb,
            "the flag must be readable immediately after being set, as a \
             restart would read it"
        );

        // A second trim on the same session (e.g. the next turn overflows
        // again) must leave the flag true, not toggle or duplicate it.
        store.set_trim_breadcrumb("sess-trimmed", true).unwrap();
        let restored_again = match store.load_session_for_restore("sess-trimmed").unwrap() {
            AcpSessionRestore::Restorable(data) => data,
            AcpSessionRestore::Missing => panic!("expected a restorable session, got Missing"),
            AcpSessionRestore::Killed => panic!("expected a restorable session, got Killed"),
        };
        assert!(
            restored_again.trim_breadcrumb,
            "the breadcrumb flag must remain true across a second trim"
        );

        // Clearing it (e.g. `clear_history`) must be independently observable.
        store.set_trim_breadcrumb("sess-trimmed", false).unwrap();
        let restored_cleared = match store.load_session_for_restore("sess-trimmed").unwrap() {
            AcpSessionRestore::Restorable(data) => data,
            AcpSessionRestore::Missing => panic!("expected a restorable session, got Missing"),
            AcpSessionRestore::Killed => panic!("expected a restorable session, got Killed"),
        };
        assert!(
            !restored_cleared.trim_breadcrumb,
            "the flag must be explicitly clearable and not re-inferred from history text"
        );
    }
}
