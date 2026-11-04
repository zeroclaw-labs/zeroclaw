//! Legacy ACP schema reader for installation upgrades and old-schema fixtures.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
#[cfg(any(test, feature = "test-util"))]
use rusqlite::{Transaction, TransactionBehavior};
use std::path::Path;
use zeroclaw_api::media::AttachmentReference;
#[cfg(any(test, feature = "test-util"))]
use zeroclaw_api::model_provider::projected_entry_count;
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage, ToolCall, ToolResultMessage};
#[cfg(any(test, feature = "test-util"))]
use zeroclaw_api::plan::PlanEntry;
#[cfg(any(test, feature = "test-util"))]
use zeroclaw_api::session_events::transcript::bounded_transcript_messages;
use zeroclaw_api::session_events::transcript::provider_safe_history;
#[cfg(any(test, feature = "test-util"))]
use zeroclaw_log::{Action, EventOutcome};

use crate::session_repository::SessionTranscript as AcpSessionData;
#[cfg(any(test, feature = "test-util"))]
use crate::session_repository::{
    SessionKillTransition as AcpSessionKillTransition, SessionListEntry as AcpSessionSummary,
    SessionMessagePage as AcpSessionPage, SessionOwnership as AcpSessionAccess,
    SessionRestore as AcpSessionRestore,
};

/// Internal discriminator for `acp_tool_calls.event_kind`. The 'in' row
/// records the call args; the 'out' row records the result. Two append-only
/// rows per call, correlated by the provider-issued `tool_call_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(test, feature = "test-util"))]
enum ToolEventKind {
    In,
    Out,
}

#[cfg(any(test, feature = "test-util"))]
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
#[cfg(any(test, feature = "test-util"))]
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

pub struct LegacyAcpStore {
    conn: Mutex<Connection>,
    // Resolve the installation's canonical format fence on each transcript write.
    sessions_dir: std::path::PathBuf,
    importing: bool,
}

/// The sessions one projected-count refresh pass may score: the scope of
/// the listing about to run, or the one session a count read names. Scoring
/// stays scoped to the rows that listing returns, before any caller-side
/// principal filter, so an agent-scoped listing never rescores the sessions
/// of other agents.
#[cfg(any(test, feature = "test-util"))]
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

#[cfg(any(test, feature = "test-util"))]
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
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[cfg(any(test, feature = "test-util"))]
struct AcpSessionCursor {
    version: u8,
    session_id: i64,
    max_message_id: i64,
    next_message_id: i64,
    next_entry_offset: Option<usize>,
}

#[cfg(any(test, feature = "test-util"))]
const ACP_SESSION_CURSOR_VERSION: u8 = 1;
#[cfg(any(test, feature = "test-util"))]
const ACP_SESSION_MAX_PAGE_SIZE: usize = 1_000;

impl LegacyAcpStore {
    pub(crate) fn legacy_metadata(
        &self,
        session_uuid: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let conn = self.legacy_connection()?;
        let (workspace, surface, tokens, plan): (String, Option<String>, i64, Option<String>) = conn.query_row(
            "SELECT workspace_dir, interaction_surface, token_count, plan_json FROM acp_sessions WHERE session_uuid=?1",
            params![session_uuid], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let mut fields = vec![
            ("workspace_dir".into(), serde_json::json!(workspace)),
            ("interaction_surface".into(), serde_json::json!(surface)),
            (
                "latest_context_input_tokens".into(),
                serde_json::json!(tokens.max(0) as u64),
            ),
            (
                "plan".into(),
                match plan {
                    Some(plan) => serde_json::from_str(&plan)?,
                    None => serde_json::json!([]),
                },
            ),
        ];
        let mut statement = conn.prepare("SELECT e.action,e.outcome,e.payload,e.created_at FROM acp_session_events e JOIN acp_sessions s ON s.id=e.session_id WHERE s.session_uuid=?1 ORDER BY e.id")?;
        for row in statement.query_map(params![session_uuid], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })? {
            let (action, outcome, payload, created_at) = row?;
            fields.push(("protocol_observation".into(), serde_json::json!({"action":action,"outcome":outcome,"payload":payload,"created_at":created_at})));
        }
        Ok(fields)
    }

    pub(crate) fn legacy_checkpoint(
        &self,
        session_uuid: &str,
    ) -> Result<Option<serde_json::Value>> {
        let conn = self.legacy_connection()?;
        let checkpoint: Option<(i64, String, Option<i64>)> = conn.query_row(
            "SELECT c.session_id,c.turn_id,s.retained_context_frontier FROM acp_turn_checkpoints c JOIN acp_sessions s ON s.id=c.session_id WHERE s.session_uuid=?1",
            params![session_uuid], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let Some((session, turn, frontier)) = checkpoint else {
            return Ok(None);
        };
        let mut statement = conn.prepare(
            "SELECT payload FROM acp_turn_checkpoint_events WHERE session_id=?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![session], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let fragments = rows
            .into_iter()
            .map(|raw| serde_json::from_str::<ConversationMessage>(&raw))
            .collect::<serde_json::Result<Vec<_>>>()?;
        Ok(Some(
            serde_json::json!({"turn_id":turn,"retained_context_frontier":frontier,"fragments":fragments}),
        ))
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        let guard = crate::session_events::installation::InstallationGuard::acquire(&sessions_dir)?;
        guard.require_legacy()?;
        Self::open_legacy(&sessions_dir, false)
    }

    pub(crate) fn open_import(
        guard: &crate::session_events::installation::InstallationGuard,
    ) -> Result<Self> {
        Self::open_legacy(guard.directory(), true)
    }

    fn legacy_connection(&self) -> Result<parking_lot::MutexGuard<'_, Connection>> {
        Ok(self.conn.lock())
    }

    fn open_legacy(sessions_dir: &Path, importing: bool) -> Result<Self> {
        std::fs::create_dir_all(sessions_dir).context("Failed to create sessions directory")?;
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
                 attachments_json TEXT,
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
                 attachments_json TEXT,
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
                 turn_id       TEXT NOT NULL
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

        Self::ensure_message_attachments_column(&conn)
            .context("Failed to migrate ACP message attachments")?;
        Self::ensure_tool_attachments_column(&conn)
            .context("Failed to migrate ACP tool attachments")?;

        Ok(Self {
            conn: Mutex::new(conn),
            sessions_dir: sessions_dir.into(),
            importing,
        })
    }

    fn ensure_message_attachments_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(acp_messages)")?;
        let mut rows = stmt.query([])?;
        let mut present = false;
        while let Some(row) = rows.next()? {
            if row.get::<_, String>(1)? == "attachments_json" {
                present = true;
                break;
            }
        }
        drop(rows);
        drop(stmt);
        if !present {
            conn.execute(
                "ALTER TABLE acp_messages ADD COLUMN attachments_json TEXT",
                [],
            )?;
        }
        Ok(())
    }

    fn ensure_tool_attachments_column(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(acp_tool_calls)")?;
        let mut rows = stmt.query([])?;
        let mut present = false;
        while let Some(row) = rows.next()? {
            if row.get::<_, String>(1)? == "attachments_json" {
                present = true;
                break;
            }
        }
        drop(rows);
        drop(stmt);
        if !present {
            conn.execute(
                "ALTER TABLE acp_tool_calls ADD COLUMN attachments_json TEXT",
                [],
            )?;
        }
        Ok(())
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
    #[cfg(any(test, feature = "test-util"))]
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn create_session_with_interaction_surface(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
        interaction_surface: Option<&str>,
        principal_id: Option<&str>,
    ) -> Result<i64> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn delete_session_owned(
        &self,
        session_uuid: &str,
        owner_principal_id: &str,
    ) -> Result<bool> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn session_principal(&self, session_uuid: &str) -> Result<Option<Option<String>>> {
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn session_owner_and_surface(
        &self,
        session_uuid: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn bind_interaction_surface_if_unset(
        &self,
        session_uuid: &str,
        interaction_surface: &str,
    ) -> Result<String> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
        // Legacy reads may repair derived columns. Import already owns this
        // lock; public reads must fence those writes against installation cutover.
        let _installation = (!self.importing)
            .then(|| crate::session_events::installation::legacy_guard(&self.sessions_dir))
            .transpose()?;
        let conn = self.legacy_connection()?;

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

        let messages = Self::load_messages(&conn, session_id)?;
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
            latest_context_input_tokens: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
            trim_breadcrumb,
            retained_context,
            principal_id,
        }))
    }

    /// Load one bounded page of the projected ACP transcript. Cursor reads
    /// hydrate only the durable groups needed for this page; the cursor's
    /// message bound makes later appends invisible to an established walk.
    #[cfg(any(test, feature = "test-util"))]
    pub fn load_message_page(
        &self,
        session_uuid: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<AcpSessionPage> {
        // Cursor validation and page hydration must share one snapshot so a
        // concurrent transcript replacement cannot create false exhaustion.
        let mut conn = self.legacy_connection()?;
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
                    "SELECT id, role, content, reasoning_content, attachments_json
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
            let Some((message_id, role, content, reasoning_content, attachments_json)) = row else {
                break;
            };
            let group = load_projected_group(
                &conn,
                message_id,
                role,
                content,
                reasoning_content,
                parse_attachments(attachments_json)?,
            )?;
            let end = end_offset.unwrap_or(group.len());
            if end > group.len() {
                return Err(anyhow::Error::msg("invalid ACP session cursor offset"));
            }
            if group.len() == 0 {
                group.ensure_well_formed(&conn)?;
            }
            let start = end.saturating_sub(remaining);
            reverse_page.push(group.entries_to_messages(
                &conn,
                start..end,
                session_id,
                state.max_message_id,
                message_id,
            )?);
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
        for group in reverse_page.into_iter().rev() {
            messages.extend(group);
        }
        let next_cursor = next.map(encode_cursor).transpose()?;
        let has_older = next_cursor.is_some();
        Ok(AcpSessionPage {
            principal_id,
            messages,
            next_cursor,
            has_older,
        })
    }

    /// Load a durable ACP transcript only when both its UUID and owning agent
    /// match. Keeping the alias predicate in SQL makes an unknown UUID and a
    /// UUID owned by another agent indistinguishable to callers.
    /// Killed sessions remain readable as history; killing prevents runtime
    /// rehydration, not access to retained transcripts by their owner.
    #[cfg(any(test, feature = "test-util"))]
    pub fn load_session_for_agent(
        &self,
        session_uuid: &str,
        agent_alias: &str,
    ) -> Result<Option<AcpSessionData>> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;

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
        let messages = Self::load_messages(&conn, session_id)?;
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
            latest_context_input_tokens: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
            trim_breadcrumb,
            retained_context,
            principal_id,
        }))
    }

    /// Classify an exact ACP session key without exposing the foreign owner.
    /// Callers use `Foreign` and `Missing` to make the same fail-closed response
    /// while still avoiding an unsafe fallback to another session backend.
    #[cfg(any(test, feature = "test-util"))]
    pub fn classify_session_for_agent(
        &self,
        session_uuid: &str,
        agent_alias: &str,
    ) -> Result<AcpSessionAccess> {
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn is_live_session_for_agent(&self, session_uuid: &str, agent_alias: &str) -> Result<bool> {
        let conn = self.legacy_connection()?;
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
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn load_session_for_restore(&self, session_uuid: &str) -> Result<AcpSessionRestore> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;

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
        let messages = Self::load_messages(&conn, session_id)?;
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
            latest_context_input_tokens: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn list_sessions(&self) -> Result<Vec<AcpSessionSummary>> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
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
                latest_context_input_tokens: token_count.max(0) as u64,
                message_count: msg_count.max(0) as usize,
            });
        }
        Ok(out)
    }

    /// List live ACP sessions owned by `agent_alias`, ordered by most recent
    /// activity. Killed rows are omitted from live discovery, but their retained
    /// transcripts remain readable through `load_session_for_agent` and they
    /// remain available to export through `list_sessions_by_agent`.
    #[cfg(any(test, feature = "test-util"))]
    pub fn list_live_sessions_by_agent(&self, agent_alias: &str) -> Result<Vec<AcpSessionSummary>> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
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
                latest_context_input_tokens: token_count.max(0) as u64,
                message_count: msg_count.max(0) as usize,
            });
        }
        Ok(out)
    }

    fn load_messages(conn: &Connection, session_id: i64) -> Result<Vec<ConversationMessage>> {
        // Pull all message rows, excluding any `system` row. `insert_messages`
        // has never written one since the write-path filter that keeps the
        // Agent's system prompt out of authoritative replacements, but a
        // database created before that fix can still have one on disk;
        // filtering here is defense in depth so a restored session or a
        // `session/messages` read can't re-expose it even if a write path
        // regresses or an old row survives a partial migration.
        let mut msg_stmt = conn
            .prepare(
                "SELECT id, role, content, reasoning_content, attachments_json
                 FROM acp_messages WHERE session_id = ?1 AND role != 'system' ORDER BY id ASC",
            )
            .context("Failed to prepare message query")?;

        let msg_rows = msg_stmt
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
                "SELECT tool_call_id, tool_name, event_kind, payload, attachments_json
                 FROM acp_tool_calls WHERE message_id = ?1 ORDER BY id ASC",
            )
            .context("Failed to prepare tool_call query")?;

        let mut out = Vec::with_capacity(msg_rows.len());
        for (msg_id, role, content, reasoning_content, attachments_json) in msg_rows {
            let attachments = parse_attachments(attachments_json)?;
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
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to read tool_call rows")?;
            for (tool_call_id, tool_name, event_kind, payload, attachments_json) in rows {
                match event_kind.as_str() {
                    "in" => ins.push(ToolCall {
                        id: tool_call_id,
                        name: tool_name,
                        arguments: payload,
                        extra_content: None,
                    }),
                    "out" => outs.push(ToolResultMessage {
                        success: None,
                        tool_call_id,
                        content: payload,
                        // Carry the producing tool name (looked up from the
                        // matching 'in' row on write) so a resumed session
                        // retains the producing tool's identity.
                        tool_name,
                        attachments: parse_attachments(attachments_json)?,
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
                out.push(ConversationMessage::Chat(ChatMessage {
                    timestamp: None,
                    role,
                    content,
                    attachments,
                    tool_calls: Vec::new(),
                    tool_result: None,
                    reasoning_content: None,
                }));
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
        }

        Ok(out)
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
    #[cfg(any(test, feature = "test-util"))]
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
        Ok(Some(provider_safe_history(&record.messages)))
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
    #[cfg(any(test, feature = "test-util"))]
    fn insert_messages(
        tx: &Transaction<'_>,
        session_id: i64,
        messages: &[ConversationMessage],
        now: &str,
    ) -> Result<i64> {
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
                    tx.execute(
                        "INSERT INTO acp_messages
                           (session_id, role, content, reasoning_content, attachments_json, created_at)
                         VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
                        params![
                            session_id,
                            chat.role,
                            chat.content,
                            serde_json::to_string(&chat.attachments)?,
                            now,
                        ],
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
                               (message_id, tool_call_id, tool_name, event_kind, payload, outcome, attachments_json, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                msg_id,
                                result.tool_call_id,
                                tool_name,
                                ToolEventKind::Out.as_str(),
                                result.content,
                                EventOutcome::Unknown.as_str(),
                                serde_json::to_string(&result.attachments)?,
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
    #[cfg(any(test, feature = "test-util"))]
    fn append_messages(
        tx: &Transaction<'_>,
        session_uuid: &str,
        session_id: i64,
        messages: &[ConversationMessage],
        now: &str,
    ) -> Result<()> {
        let messages = bounded_transcript_messages(messages);
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
        let projected_increment = Self::insert_messages(tx, session_id, &messages, now)?;
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

    #[cfg(any(test, feature = "test-util"))]
    fn append_checkpoint_visible_messages(
        tx: &Transaction<'_>,
        session_uuid: &str,
        session_id: i64,
        messages: &[ConversationMessage],
        now: &str,
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
                        timestamp: None,
                        tool_calls: Vec::new(),
                        tool_result: None,
                        reasoning_content: None,
                        role: SYNTHETIC_INTERRUPTION_ROLE.to_string(),
                        content: chat.content.clone(),

                        attachments: Vec::new(),
                    })
                }
                other => other.clone(),
            })
            .collect::<Vec<_>>();
        Self::append_messages(tx, session_uuid, session_id, &messages, now)
    }

    #[cfg(any(test, feature = "test-util"))]
    fn session_id(conn: &Connection, session_uuid: &str) -> Result<i64> {
        conn.query_row(
            "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| row.get(0),
        )
        .with_context(|| format!("unknown session_uuid: {session_uuid}"))
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn contains_session(&self, session_uuid: &str) -> Result<bool> {
        let conn = self.legacy_connection()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM acp_sessions WHERE session_uuid = ?1)",
            params![session_uuid],
            |row| row.get(0),
        )
        .context("Failed to check ACP session existence")
    }

    /// Append all ConversationMessages from one completed turn in one transaction.
    #[cfg(any(test, feature = "test-util"))]
    pub fn append_turn(&self, session_uuid: &str, messages: &[ConversationMessage]) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        if messages.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut conn = self.legacy_connection()?;
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction()
            .context("Failed to begin append_turn transaction")?;
        Self::append_messages(&tx, session_uuid, session_id, messages, &now)?;

        tx.commit().context("Failed to commit append_turn")?;
        Ok(())
    }

    /// Replace a session's visible transcript while keeping message identity
    /// local to the transcript. Native retained provider context uses a
    /// separate projection and never calls this method for a trim.
    #[cfg(any(test, feature = "test-util"))]
    pub fn replace_messages(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        self.replace_messages_inner(session_uuid, messages, None)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn replace_messages_and_breadcrumb(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
        breadcrumb_present: bool,
    ) -> Result<()> {
        self.replace_messages_inner(session_uuid, messages, Some(breadcrumb_present))
    }

    #[cfg(any(test, feature = "test-util"))]
    fn replace_messages_inner(
        &self,
        session_uuid: &str,
        messages: &[ConversationMessage],
        breadcrumb_present: Option<bool>,
    ) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.legacy_connection()?;
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
        let projected_count = Self::insert_messages(&tx, session_id, messages, &now)?;
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

    #[cfg(any(test, feature = "test-util"))]
    pub fn begin_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction()
            .context("Failed to begin ACP turn checkpoint transaction")?;
        tx.execute(
            "INSERT INTO acp_turn_checkpoints (session_id, turn_id) VALUES (?1, ?2)",
            params![session_id, turn_id],
        )
        .context("Failed to begin ACP turn checkpoint")?;
        Self::append_checkpoint_events(&tx, session_id, messages)?;
        tx.commit()
            .context("Failed to commit ACP turn checkpoint")?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn append_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<i64> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
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

    #[cfg(any(test, feature = "test-util"))]
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
        for message in bounded_transcript_messages(messages) {
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

    /// Persist an owner-observed checkpoint frontier. The frontier is supplied
    /// by the serial event consumer after its append transaction; it is never
    /// inferred by content comparison or by a delayed global MAX in this path.
    #[cfg(any(test, feature = "test-util"))]
    pub fn persist_retained_context_at_frontier(
        &self,
        session_uuid: &str,
        turn_id: &str,
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
        frontier: i64,
    ) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin retained context transaction")?;
        Self::ensure_active_checkpoint(&tx, session_id, turn_id)?;
        let retained = bounded_transcript_messages(retained_messages);
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn persist_retained_context_seed(
        &self,
        session_uuid: &str,
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
    ) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
        let session_id = Self::session_id(&conn, session_uuid)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin retained seed context transaction")?;
        let retained = bounded_transcript_messages(retained_messages);
        let record = RetainedContextRecord {
            messages: Self::without_hidden_reasoning(&provider_safe_history(&retained)),
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

    #[cfg(any(test, feature = "test-util"))]
    pub fn checkpoint_frontier(&self, session_uuid: &str, turn_id: &str) -> Result<i64> {
        let conn = self.legacy_connection()?;
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

    #[cfg(any(test, feature = "test-util"))]
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

    #[cfg(any(test, feature = "test-util"))]
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
    #[cfg(any(test, feature = "test-util"))]
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn finalize_turn_checkpoint_with_context_for_owner(
        &self,
        session_uuid: &str,
        turn_id: &str,
        terminal_messages: &[ConversationMessage],
        retained_messages: &[ConversationMessage],
        breadcrumb: bool,
        owner_principal_id: Option<&str>,
    ) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.legacy_connection()?;
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
            bounded_transcript_messages(terminal_messages)
        };
        Self::append_checkpoint_visible_messages(&tx, session_uuid, session_id, &visible, &now)?;
        let retained = bounded_transcript_messages(retained_messages);
        let record = RetainedContextRecord {
            messages: Self::without_hidden_reasoning(&provider_safe_history(&retained)),
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

    #[cfg(any(test, feature = "test-util"))]
    pub fn discard_turn_checkpoint(&self, session_uuid: &str, turn_id: &str) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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

    #[cfg(any(test, feature = "test-util"))]
    pub fn finalize_turn_checkpoint(
        &self,
        session_uuid: &str,
        turn_id: &str,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.legacy_connection()?;
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
        let messages = bounded_transcript_messages(messages);
        Self::append_messages(&tx, session_uuid, session_id, &messages, &now)?;
        tx.execute(
            "DELETE FROM acp_turn_checkpoints WHERE session_id = ?1 AND turn_id = ?2",
            params![session_id, turn_id],
        )
        .context("Failed to delete finalized ACP turn checkpoint")?;
        tx.commit()
            .context("Failed to commit ACP turn finalization")?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-util"))]
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn recover_turn_checkpoint_for_owner(
        &self,
        session_uuid: &str,
        interruption_marker: &str,
        owner_principal_id: Option<&str>,
    ) -> Result<bool> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.legacy_connection()?;
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
        let visible = bounded_transcript_messages(&Self::fold_checkpoint_fragments(fragments));
        Self::append_messages(&tx, session_uuid, session_id, &visible, &now)?;
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
            record.messages = provider_safe_history(&Self::fold_checkpoint_fragments(projected));
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

    #[cfg(any(test, feature = "test-util"))]
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

    #[cfg(any(test, feature = "test-util"))]
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_token_count(&self, session_uuid: &str, token_count: u64) -> Result<()> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn clear_token_count(&self, session_uuid: &str) -> Result<()> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_trim_breadcrumb(&self, session_uuid: &str, present: bool) -> Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_plan(&self, session_uuid: &str, entries: &[PlanEntry]) -> Result<()> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let plan_json =
            serde_json::to_string(entries).context("Failed to serialize plan entries")?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn get_plan(&self, session_uuid: &str) -> Result<Vec<PlanEntry>> {
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn append_event(
        &self,
        session_uuid: &str,
        action: Action,
        outcome: EventOutcome,
        payload: Option<&str>,
    ) -> Result<()> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn delete_session(&self, session_uuid: &str) -> Result<bool> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn count_live_sessions_by_agent(&self, agent_alias: &str) -> Result<usize> {
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn list_sessions_by_agent(&self, agent_alias: &str) -> Result<Vec<AcpSessionSummary>> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
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
                latest_context_input_tokens: token_count.max(0) as u64,
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn projected_message_count(&self, session_uuid: &str) -> Result<Option<usize>> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let mut conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn delete_sessions_by_agent(&self, agent_alias: &str) -> Result<usize> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn rename_sessions_by_agent(&self, from: &str, to: &str) -> Result<usize> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn mark_session_killed_atomic(
        &self,
        session_uuid: &str,
    ) -> Result<AcpSessionKillTransition> {
        self.mark_session_killed_atomic_for_owner(session_uuid, None)
    }

    /// Tombstone only the durable row still owned by this principal. The
    /// update predicate and transition inspection share one transaction.
    #[cfg(any(test, feature = "test-util"))]
    pub fn mark_session_killed_atomic_for_owner(
        &self,
        session_uuid: &str,
        owner_principal_id: Option<&str>,
    ) -> Result<AcpSessionKillTransition> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
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
        let conn = self.legacy_connection()?;
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
    #[cfg(any(test, feature = "test-util"))]
    pub fn touch_session(&self, session_uuid: &str) -> Result<()> {
        let _installation = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.legacy_connection()?;
        conn.execute(
            "UPDATE acp_sessions SET last_activity = ?1 WHERE session_uuid = ?2",
            params![now, session_uuid],
        )
        .context("Failed to touch ACP session")?;
        Ok(())
    }
}

#[cfg(any(test, feature = "test-util"))]
struct ProjectedGroup {
    message_id: i64,
    role: String,
    content: String,
    reasoning_content: Option<String>,
    attachments: Vec<AttachmentReference>,
    has_tool_events: bool,
    input_count: usize,
    unmatched_output_count: usize,
}

fn parse_attachments(raw: Option<String>) -> Result<Vec<AttachmentReference>> {
    raw.filter(|value| !value.is_empty())
        .map(|value| serde_json::from_str(&value).context("invalid persisted attachment list"))
        .transpose()
        .map(|attachments| attachments.unwrap_or_default())
}

#[cfg(any(test, feature = "test-util"))]
impl ProjectedGroup {
    #[cfg(any(test, feature = "test-util"))]
    fn len(&self) -> usize {
        if !self.has_tool_events {
            1
        } else {
            usize::from(!self.content.is_empty()) + self.input_count + self.unmatched_output_count
        }
    }

    #[cfg(any(test, feature = "test-util"))]
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

    #[cfg(any(test, feature = "test-util"))]
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
                    timestamp: None,
                    tool_calls: Vec::new(),
                    tool_result: None,
                    reasoning_content: None,
                    role: if self.role == SYNTHETIC_INTERRUPTION_ROLE {
                        "system".to_string()
                    } else {
                        self.role.clone()
                    },
                    content: self.content.clone(),
                    attachments: self.attachments.clone(),
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
                timestamp: None,
                tool_calls: Vec::new(),
                tool_result: None,
                reasoning_content: None,
                role: "assistant".to_string(),
                content: self.content.clone(),
                attachments: self.attachments.clone(),
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
                        output_rows.tool_name, output_rows.attachments_json
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
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )?;
            for row in rows {
                let (tool_call_id, tool_name, arguments, output, output_name, output_attachments) =
                    row?;
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
                        success: None,
                        tool_call_id,
                        content,
                        tool_name: output_name.unwrap_or_default(),
                        attachments: parse_attachments(output_attachments)?,
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
                        output_rows.payload, output_rows.attachments_json
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
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )?;
            for row in rows {
                let (tool_call_id, tool_name, content, attachments_json) = row?;
                selected_ids.insert(tool_call_id.clone());
                messages.push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                    success: None,
                    tool_call_id,
                    content,
                    tool_name,
                    attachments: parse_attachments(attachments_json)?,
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

#[cfg(any(test, feature = "test-util"))]
fn load_projected_group(
    conn: &Connection,
    message_id: i64,
    role: String,
    content: String,
    reasoning_content: Option<String>,
    attachments: Vec<AttachmentReference>,
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
            attachments,
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
        attachments,
        has_tool_events,
        input_count: input_count.max(0) as usize,
        unmatched_output_count: unmatched_output_count.max(0) as usize,
    })
}

#[cfg(any(test, feature = "test-util"))]
fn encode_cursor(cursor: AcpSessionCursor) -> Result<String> {
    let bytes = serde_json::to_vec(&cursor)?;
    let mut encoded = String::from("acp1.");
    for byte in bytes {
        use std::fmt::Write;
        write!(encoded, "{byte:02x}")?;
    }
    Ok(encoded)
}

#[cfg(any(test, feature = "test-util"))]
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
    use zeroclaw_api::model_provider::ChatMessage;

    fn open_store() -> (TempDir, LegacyAcpStore) {
        let tmp = TempDir::new().unwrap();
        let store = LegacyAcpStore::new(tmp.path()).unwrap();
        (tmp, store)
    }

    #[test]
    fn native_acp_fence_blocks_old_transcript_handles_and_keeps_protocol_metadata_writable() {
        use crate::session_events::installation::NativeBackend;
        let (directory, store) = open_store();
        store
            .create_session("fixture", "agent", "/tmp", None)
            .expect("protocol");
        let user = ConversationMessage::Chat(ChatMessage::user("fixture"));
        store
            .begin_turn_checkpoint("fixture", "old-turn", std::slice::from_ref(&user))
            .expect("legacy checkpoint");
        crate::session_events::upgrade::upgrade_installation(
            directory.path(),
            NativeBackend::Sqlite,
            |_| "agent".into(),
        )
        .expect("fence");
        assert!(
            store
                .append_turn("fixture", std::slice::from_ref(&user))
                .is_err()
        );
        assert!(
            store
                .replace_messages("fixture", std::slice::from_ref(&user))
                .is_err()
        );
        assert!(
            store
                .append_turn_checkpoint("fixture", "old-turn", std::slice::from_ref(&user))
                .is_err()
        );
        assert!(
            store
                .recover_turn_checkpoint("fixture", "interrupted")
                .is_err()
        );
        assert!(
            store
                .persist_retained_context_seed("fixture", std::slice::from_ref(&user), false)
                .is_err()
        );
        let store = crate::session_repository::SessionRepository::new(directory.path())
            .expect("standard repository");
        store.touch_session("fixture").expect("protocol activity");
        store.set_plan("fixture", &[]).expect("protocol plan");
        store
            .create_session("unrelated", "acp", "agent", "/tmp", None)
            .expect("unrelated");
        assert!(
            store.append_turn("unrelated", &[user]).is_err(),
            "all legacy transcript writers are fenced after installation upgrade"
        );
    }
}
