//! ACP session persistence.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::Path;
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage, ToolCall, ToolResultMessage};
use zeroclaw_api::plan::PlanEntry;
use zeroclaw_log::{Action, EventOutcome};

const MAX_PERSISTED_TOOL_OUTPUT_BYTES: usize = 16 * 1024;
/// Fixed transcript marker appended after a turn that ended in an agent or
/// provider error, stored as a `role == "system"` chat row. System rows are
/// excluded from provider replay on restore (`Agent::seed_conversation_history_with_event`
/// skips them), so this marker is the durable boundary of the failed turn,
/// not something the next provider request sees. Distinct from the localized
/// interrupted-turn markers (assistant text), so the two are tellable apart
/// in the transcript.
pub const FAILED_TURN_MARKER: &str = "turn failed";

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
}

/// Kind of a settled terminal append range recorded in the
/// `acp_terminal_ranges` table.
///
/// Every range row certifies that its message rows were written by one
/// completed transaction — the write settled. The kind keeps what the turn
/// actually was distinguishable: a normally finished turn, a turn that
/// terminated in failure (its batch carries the fixed failed-turn marker), or
/// a turn rescued from an interruption by checkpoint recovery. Only
/// `Completed` ranges may ever be certified as compactable coverage; `Failed`
/// and `Interrupted` ranges stay raw tail history instead of being silently
/// summarized as successful work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRangeKind {
    Completed,
    Failed,
    Interrupted,
}

impl TerminalRangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_persisted(value: &str) -> Result<Self> {
        match value {
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            other => Err(anyhow::Error::msg(format!(
                "unknown terminal_kind '{other}' in acp_terminal_ranges"
            ))),
        }
    }

    /// Classify a terminal append batch from its own content. A batch whose
    /// final row is the store-owned failed-turn marker is a terminal failed
    /// attempt; anything else written by `append_turn` or turn finalization
    /// is a normally completed turn.
    fn for_terminal_batch(messages: &[ConversationMessage]) -> Self {
        match messages.last() {
            Some(ConversationMessage::Chat(chat))
                if chat.role == "system" && chat.content == FAILED_TURN_MARKER =>
            {
                Self::Failed
            }
            _ => Self::Completed,
        }
    }
}

pub struct AcpSessionStore {
    conn: Mutex<Connection>,
}

pub struct AcpSessionData {
    pub session_uuid: String,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub interaction_surface: Option<String>,
    pub token_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<ConversationMessage>,
}

pub enum AcpSessionRestore {
    Missing,
    Killed,
    Restorable(AcpSessionData),
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
    pub agent_alias: String,
    pub workspace_dir: String,
    pub token_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_count: usize,
}

/// One settled terminal append range, as recorded by the transaction that
/// wrote its message rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpTerminalRangeRow {
    pub first_message_id: i64,
    pub last_message_id: i64,
    pub kind: TerminalRangeKind,
}

/// The single active derived compaction checkpoint for a session.
///
/// This is derived data: the original messages it covers are retained
/// untouched, and the checkpoint only describes a provider-facing projection.
/// Estimates are display data; the row ids and counts are the integrity
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpActiveCheckpointRecord {
    pub format_version: i64,
    pub operation_id: String,
    pub source_first_message_id: i64,
    pub covered_through_message_id: i64,
    pub source_message_rows: i64,
    pub summary: String,
    pub summary_model_provider: String,
    pub summary_model: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub created_at: String,
}

/// State of the most recent checkpoint row written for one operation id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpCheckpointOperationState {
    Active,
    Inactive,
}

/// Consistent single-snapshot view of everything a manual compaction
/// operation needs: the durable session incarnation, its original rows with
/// stable ids, the settled terminal ranges, the active checkpoint (if any),
/// the caller's own operation's checkpoint row (if any), and any durable
/// in-flight turn checkpoint.
pub struct AcpCompactionSnapshot {
    /// Autoincrement row id of the session: the durable incarnation identity.
    /// A delete/recreate of the same public UUID produces a new id, so a
    /// stale checkpoint or retry can never attach to the successor.
    pub session_row_id: i64,
    pub session_uuid: String,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub interaction_surface: Option<String>,
    pub killed: bool,
    pub message_rows: Vec<(i64, ConversationMessage)>,
    pub terminal_ranges: Vec<AcpTerminalRangeRow>,
    pub active_checkpoint: Option<AcpActiveCheckpointRecord>,
    pub operation_checkpoint: Option<AcpCheckpointOperationState>,
    pub inflight_turn_id: Option<String>,
}

/// Restore-mode read that pairs the full durable originals with the active
/// checkpoint (if any) so callers assemble one committed projection.
pub struct AcpProjectedRestore {
    pub data: AcpSessionData,
    pub message_rows: Vec<(i64, ConversationMessage)>,
    pub checkpoint: Option<AcpActiveCheckpointRecord>,
}

pub enum AcpSessionRestoreProjection {
    Missing,
    Killed,
    Projected(Box<AcpProjectedRestore>),
}

/// Selected contiguous known-completed prefix coverage for a compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSourceSelection {
    pub first_message_id: i64,
    pub covered_through_message_id: i64,
    /// Number of durable message ROWS in the covered span (not decomposed
    /// message entries).
    pub covered_message_rows: usize,
    pub covered_ranges: usize,
}

/// Why a session's history cannot be compacted in v1. Each variant is a
/// typed, user-explainable refusal — compaction never silently certifies
/// unknown, interrupted, failed, or ambiguous history as recoverable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionSourceError {
    /// No terminal ranges certify the session's head rows: the history is
    /// empty or predates terminal-range recording (legacy).
    NoTerminalCoverage { first_message_id: i64 },
    /// The head range settled, but not as a completed turn (failed or
    /// interrupted); coverage cannot start there.
    LeadingRangeNotCompleted {
        kind: TerminalRangeKind,
        first_message_id: i64,
    },
    /// The only certified coverage is the newest completed turn, which is
    /// always retained.
    NewestTurnMustBeRetained,
    /// The candidate coverage contains a tool exchange whose call/result
    /// pairing is ambiguous or unpaired.
    AmbiguousToolPairing { message_id: i64 },
}

/// Durably-validated request to activate (or idempotently recognize) one
/// compaction checkpoint.
pub struct CompactionActivationRequest<'a> {
    pub session_uuid: &'a str,
    pub expected_session_row_id: i64,
    pub format_version: i64,
    pub operation_id: &'a str,
    /// The active checkpoint operation this request snapshotted before
    /// summarizing (`None` when none was active). Activation supersedes a
    /// prior checkpoint only when the durable active operation still
    /// matches this expectation, so a stale request can never overwrite a
    /// later operation's committed projection.
    pub expected_prior_active_operation: Option<&'a str>,
    pub source_first_message_id: i64,
    pub covered_through_message_id: i64,
    pub source_message_rows: i64,
    pub summary: &'a str,
    pub summary_model_provider: &'a str,
    pub summary_model: &'a str,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionActivationOutcome {
    /// The checkpoint row was committed and is now the active projection.
    Activated,
    /// The same operation id is already the active checkpoint: a committed
    /// retry is recognized without a second write. The caller rebuilds its
    /// acknowledgement from the active checkpoint it already read.
    AlreadyActive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionActivationError {
    SessionMissing,
    IncarnationMismatch {
        found_session_row_id: i64,
    },
    SessionKilled,
    /// A durable in-flight turn checkpoint exists; its turn has not settled.
    InflightTurn,
    /// The covered source no longer matches the snapshot the caller
    /// summarized. Nothing was written.
    SourceMismatch {
        detail: String,
    },
    /// The active checkpoint changed since the caller's snapshot; a stale
    /// request must not supersede a later operation. Nothing was written.
    StaleActiveCheckpoint {
        active_operation: Option<String>,
        expected_operation: Option<String>,
    },
    /// The activation transaction itself failed. Nothing was written.
    Storage(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionDeactivationOutcome {
    /// The active checkpoint was deactivated by this operation.
    Deactivated {
        covered_through_message_id: i64,
        covered_message_rows: i64,
    },
    /// This operation id already deactivated the checkpoint (committed
    /// retry); nothing changed.
    AlreadyDeactivated,
    /// No active checkpoint exists for this session.
    NoActiveCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionDeactivationError {
    SessionMissing,
    IncarnationMismatch {
        found_session_row_id: i64,
    },
    SessionKilled,
    InflightTurn,
    /// The active checkpoint changed since the caller's snapshot; a stale
    /// restore must not deactivate a later operation's checkpoint. Nothing
    /// was written.
    StaleActiveCheckpoint {
        active_operation: Option<String>,
        expected_operation: Option<String>,
    },
    /// The deactivation transaction itself failed. Nothing was written.
    Storage(String),
}

impl std::fmt::Display for CompactionSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTerminalCoverage { first_message_id } => write!(
                f,
                "no terminal-range coverage certifies the session head (first row {first_message_id}); \
                 legacy or empty history cannot be compacted"
            ),
            Self::LeadingRangeNotCompleted {
                kind,
                first_message_id,
            } => write!(
                f,
                "the oldest settled range (row {first_message_id}) is a {kind} turn, \
                 so no completed prefix exists to summarize",
                kind = kind.as_str()
            ),
            Self::NewestTurnMustBeRetained => write!(
                f,
                "the newest completed turn is always retained, leaving no older \
                 completed prefix to summarize"
            ),
            Self::AmbiguousToolPairing { message_id } => write!(
                f,
                "message row {message_id} has an ambiguous or unpaired tool exchange; \
                 refusing to certify it as completed coverage"
            ),
        }
    }
}

impl std::error::Error for CompactionSourceError {}

impl std::fmt::Display for CompactionActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionMissing => write!(f, "session no longer exists"),
            Self::IncarnationMismatch {
                found_session_row_id,
            } => write!(
                f,
                "session was recreated (durable row {found_session_row_id}); the stale \
                 source snapshot cannot be committed"
            ),
            Self::SessionKilled => write!(f, "session is killed"),
            Self::InflightTurn => write!(
                f,
                "a durable in-flight turn checkpoint exists; the turn must settle first"
            ),
            Self::SourceMismatch { detail } => {
                write!(f, "compaction source identity mismatch: {detail}")
            }
            Self::StaleActiveCheckpoint {
                active_operation,
                expected_operation,
            } => write!(
                f,
                "the active checkpoint changed since the snapshot (active {active:?}, \
                 expected {expected:?}); retry with a fresh operation",
                active = active_operation.as_deref().unwrap_or("<none>"),
                expected = expected_operation.as_deref().unwrap_or("<none>"),
            ),
            Self::Storage(detail) => write!(f, "compaction storage failure: {detail}"),
        }
    }
}

impl std::error::Error for CompactionActivationError {}

impl std::fmt::Display for CompactionDeactivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionMissing => write!(f, "session no longer exists"),
            Self::IncarnationMismatch {
                found_session_row_id,
            } => write!(
                f,
                "session was recreated (durable row {found_session_row_id})"
            ),
            Self::SessionKilled => write!(f, "session is killed"),
            Self::InflightTurn => write!(
                f,
                "a durable in-flight turn checkpoint exists; the turn must settle first"
            ),
            Self::StaleActiveCheckpoint {
                active_operation,
                expected_operation,
            } => write!(
                f,
                "the active checkpoint changed since the snapshot (active {active:?}, \\
                 expected {expected:?}); retry with a fresh operation",
                active = active_operation.as_deref().unwrap_or("<none>"),
                expected = expected_operation.as_deref().unwrap_or("<none>"),
            ),
            Self::Storage(detail) => write!(f, "compaction storage failure: {detail}"),
        }
    }
}

impl std::error::Error for CompactionDeactivationError {}

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
                 turn_id       TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS acp_turn_checkpoint_events (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id INTEGER NOT NULL REFERENCES acp_turn_checkpoints(session_id) ON DELETE CASCADE,
                 payload    TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acp_turn_checkpoint_events_session
                 ON acp_turn_checkpoint_events(session_id, id);

             CREATE TABLE IF NOT EXISTS acp_terminal_ranges (
                 id               INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id       INTEGER NOT NULL REFERENCES acp_sessions(id) ON DELETE CASCADE,
                 first_message_id INTEGER NOT NULL,
                 last_message_id  INTEGER NOT NULL,
                 terminal_kind    TEXT NOT NULL,
                 created_at       TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acp_terminal_ranges_session
                 ON acp_terminal_ranges(session_id, first_message_id);

             CREATE TABLE IF NOT EXISTS acp_compaction_checkpoints (
                 id                         INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id                 INTEGER NOT NULL REFERENCES acp_sessions(id) ON DELETE CASCADE,
                 format_version             INTEGER NOT NULL,
                 operation_id               TEXT NOT NULL,
                 source_first_message_id    INTEGER NOT NULL,
                 covered_through_message_id INTEGER NOT NULL,
                 source_message_rows        INTEGER NOT NULL,
                 summary                    TEXT NOT NULL,
                 summary_model_provider     TEXT NOT NULL,
                 summary_model              TEXT NOT NULL,
                 input_tokens               INTEGER,
                 output_tokens              INTEGER,
                 created_at                 TEXT NOT NULL,
                 active                     INTEGER NOT NULL DEFAULT 1,
                 deactivated_at             TEXT,
                 deactivated_by_operation   TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_acp_compaction_checkpoints_session
                 ON acp_compaction_checkpoints(session_id, id);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_acp_compaction_one_active
                 ON acp_compaction_checkpoints(session_id) WHERE active = 1;",
        )
        .context("Failed to create ACP session schema")?;

        Self::ensure_killed_at_column(&conn)
            .context("Failed to migrate ACP session killed marker")?;

        Self::ensure_plan_json_column(&conn)
            .context("Failed to migrate ACP session plan column")?;

        Self::ensure_interaction_surface_column(&conn)
            .context("Failed to migrate ACP session interaction surface")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
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

    /// Record a new session. Returns the integer `id` assigned by SQLite.
    pub fn create_session(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
    ) -> Result<i64> {
        self.create_session_with_interaction_surface(session_uuid, agent_alias, workspace_dir, None)
    }

    /// Record a session with an optional host-validated interaction surface.
    pub fn create_session_with_interaction_surface(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
        interaction_surface: Option<&str>,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)",
            params![
                session_uuid,
                agent_alias,
                workspace_dir,
                interaction_surface,
                now
            ],
        )
        .context("Failed to create ACP session")?;
        Ok(conn.last_insert_rowid())
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

    /// Load session metadata and full message history.
    ///
    /// This is the legacy reader used by external ACP `session/load` and
    /// `session/resume` consumers that have not been adapted to context
    /// compaction. It fails closed for a session with an ACTIVE compaction
    /// checkpoint: returning unprojected originals to an unsupported
    /// execution consumer would silently undo the committed projection. Use
    /// [`Self::load_session_transcript`] for intentional transcript reads and
    /// [`Self::load_session_for_restore_with_projection`] for projected
    /// native restore paths.
    pub fn load_session(&self, session_uuid: &str) -> Result<Option<AcpSessionData>> {
        let conn = self.conn.lock();

        let session_id = match conn.query_row(
            "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(session_id) => session_id,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e).context("Failed to query ACP session"),
        };

        if let Some(checkpoint) = Self::active_checkpoint_row(&conn, session_id)? {
            return Err(anyhow::Error::msg(format!(
                "ACP session {session_uuid} has an active context-compaction checkpoint \
                 (operation {}). This legacy load path is not adapted to compacted \
                 sessions; resume it through the native ZeroCode Code surface.",
                checkpoint.operation_id
            )));
        }

        Self::load_session_data(&conn, session_uuid, session_id).map(Some)
    }

    /// Raw transcript reader: durable originals for intentional transcript
    /// reads (history browsing, export), regardless of any compaction
    /// checkpoint. Compaction never rewrites these rows, so the full
    /// original transcript stays loadable at all times.
    pub fn load_session_transcript(&self, session_uuid: &str) -> Result<Option<AcpSessionData>> {
        let conn = self.conn.lock();
        let session_id = match conn.query_row(
            "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(session_id) => session_id,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e).context("Failed to query ACP session"),
        };
        Self::load_session_data(&conn, session_uuid, session_id).map(Some)
    }

    fn load_session_data(
        conn: &Connection,
        session_uuid: &str,
        session_id: i64,
    ) -> Result<AcpSessionData> {
        let row = conn.query_row(
            "SELECT agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity
             FROM acp_sessions WHERE id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        );

        let (
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count,
            created_at_s,
            last_activity_s,
        ) = match row {
            Ok(r) => r,
            Err(e) => return Err(e).context("Failed to query ACP session"),
        };

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);

        let messages = Self::load_messages(conn, session_id)?;

        Ok(AcpSessionData {
            session_uuid: session_uuid.to_string(),
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
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
                "SELECT id, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity
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
        )) = row
        else {
            return Ok(None);
        };

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);
        let messages = Self::load_messages(&conn, session_id)?;

        Ok(Some(AcpSessionData {
            session_uuid: session_uuid.to_string(),
            agent_alias: owner_alias,
            workspace_dir,
            interaction_surface,
            token_count: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
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
            "SELECT id, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity, killed_at
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

        Ok(AcpSessionRestore::Restorable(AcpSessionData {
            session_uuid: session_uuid.to_string(),
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
        }))
    }

    /// List restorable sessions as lightweight summaries, ordered by most recent
    /// activity first. This is the picker-facing read: it avoids the full
    /// message-history hydration that `load_session` performs. Killed rows keep
    /// history/export data but are terminal and must not be offered for restore.
    pub fn list_sessions(&self) -> Result<Vec<AcpSessionSummary>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.session_uuid,
                        s.agent_alias,
                        s.workspace_dir,
                        s.token_count,
                        s.created_at,
                        s.last_activity,
                        (SELECT COUNT(*) FROM acp_messages m WHERE m.session_id = s.id) AS message_count
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
            ) = row.context("Failed to read ACP session row")?;
            out.push(AcpSessionSummary {
                created_at: parse_ts(&created_s, "created_at", &session_uuid),
                last_activity: parse_ts(&activity_s, "last_activity", &session_uuid),
                session_uuid,
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
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.session_uuid,
                        s.agent_alias,
                        s.workspace_dir,
                        s.token_count,
                        s.created_at,
                        s.last_activity,
                        (SELECT COUNT(*) FROM acp_messages m WHERE m.session_id = s.id) AS message_count
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
            ) = row.context("Failed to read live ACP session row")?;
            out.push(AcpSessionSummary {
                created_at: parse_ts(&created_s, "created_at", &session_uuid),
                last_activity: parse_ts(&activity_s, "last_activity", &session_uuid),
                session_uuid,
                agent_alias: owner_alias,
                workspace_dir,
                token_count: token_count.max(0) as u64,
                message_count: msg_count.max(0) as usize,
            });
        }
        Ok(out)
    }

    fn load_messages(conn: &Connection, session_id: i64) -> Result<Vec<ConversationMessage>> {
        Ok(Self::load_message_rows(conn, session_id)?
            .into_iter()
            .map(|(_, message)| message)
            .collect())
    }

    /// Load the durable transcript as `(acp_messages.id, message)` pairs in
    /// row order. A message row carrying both tool-call and tool-result rows
    /// decomposes into its `AssistantToolCalls` and `ToolResults` entries,
    /// both stamped with that row's id, so range boundaries and per-row
    /// identity stay exact for compaction coverage.
    fn load_message_rows(
        conn: &Connection,
        session_id: i64,
    ) -> Result<Vec<(i64, ConversationMessage)>> {
        // Pull all message rows.
        let mut msg_stmt = conn
            .prepare(
                "SELECT id, role, content, reasoning_content
                 FROM acp_messages WHERE session_id = ?1 ORDER BY id ASC",
            )
            .context("Failed to prepare message query")?;

        let msg_rows: Vec<(i64, String, String, Option<String>)> = msg_stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
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
        for (msg_id, role, content, reasoning_content) in msg_rows {
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

            if ins.is_empty() && outs.is_empty() {
                // Pure chat message.
                out.push((
                    msg_id,
                    ConversationMessage::Chat(ChatMessage { role, content }),
                ));
            } else {
                if !ins.is_empty() {
                    // Assistant turn that issued tool calls. The text may be empty.
                    out.push((
                        msg_id,
                        ConversationMessage::AssistantToolCalls {
                            text: if content.is_empty() {
                                None
                            } else {
                                Some(content)
                            },
                            tool_calls: ins,
                            reasoning_content,
                        },
                    ));
                }
                if !outs.is_empty() {
                    out.push((msg_id, ConversationMessage::ToolResults(outs)));
                }
            }
        }

        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn append_messages(
        tx: &Transaction<'_>,
        session_uuid: &str,
        session_id: i64,
        messages: &[ConversationMessage],
        now: &str,
        range_kind: TerminalRangeKind,
    ) -> Result<()> {
        // Track the most recent assistant message_id so a following
        // ToolResults variant can attach its 'out' rows back to it.
        let mut last_assistant_msg_id: Option<i64> = None;
        // Row-id bounds of this batch for the terminal-range record. Both are
        // acp_messages row ids; tool-call rows live in their own table and
        // never stretch the range.
        let mut first_message_id: Option<i64> = None;
        let mut last_message_id: Option<i64> = None;

        for msg in messages {
            match msg {
                ConversationMessage::Chat(chat) => {
                    tx.execute(
                        "INSERT INTO acp_messages
                           (session_id, role, content, reasoning_content, created_at)
                         VALUES (?1, ?2, ?3, NULL, ?4)",
                        params![session_id, chat.role, chat.content, now],
                    )
                    .context("Failed to insert chat message")?;
                    let row_id = tx.last_insert_rowid();
                    first_message_id.get_or_insert(row_id);
                    last_message_id = Some(row_id);
                    if chat.role == "assistant" {
                        last_assistant_msg_id = Some(row_id);
                    }
                }
                ConversationMessage::AssistantToolCalls {
                    text,
                    tool_calls,
                    reasoning_content,
                } => {
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
                    first_message_id.get_or_insert(msg_id);
                    last_message_id = Some(msg_id);
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
                                    "session_uuid": session_uuid,
                                })),
                                "ToolResults without preceding AssistantToolCalls"
                            );
                            return Err(anyhow::Error::msg(
                                "ToolResults appeared without a preceding AssistantToolCalls \
                                 message in this turn — cannot determine parent message_id",
                            ));
                        }
                    };
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

        // Record the settled terminal range in the SAME transaction as the
        // rows it certifies: this batch's rows are known-complete coverage
        // exactly when this transaction commits. An empty batch certifies no
        // rows and records nothing.
        if let (Some(first), Some(last)) = (first_message_id, last_message_id) {
            tx.execute(
                "INSERT INTO acp_terminal_ranges
                   (session_id, first_message_id, last_message_id, terminal_kind, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, first, last, range_kind.as_str(), now],
            )
            .context("Failed to insert terminal range row")?;
        }

        tx.execute(
            "UPDATE acp_sessions SET last_activity = ?1 WHERE id = ?2",
            params![now, session_id],
        )
        .context("Failed to update last_activity")?;

        Ok(())
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
        let messages = Self::bounded_transcript_messages(messages);
        let range_kind = TerminalRangeKind::for_terminal_batch(&messages);
        Self::append_messages(&tx, session_uuid, session_id, &messages, &now, range_kind)?;

        tx.commit().context("Failed to commit append_turn")?;
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
            "INSERT INTO acp_turn_checkpoints (session_id, turn_id) VALUES (?1, ?2)",
            params![session_id, turn_id],
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
    ) -> Result<()> {
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
        Self::append_checkpoint_events(&tx, session_id, messages)?;
        tx.commit()
            .context("Failed to commit ACP turn checkpoint append")?;
        Ok(())
    }

    fn append_checkpoint_events(
        tx: &Transaction<'_>,
        session_id: i64,
        messages: &[ConversationMessage],
    ) -> Result<()> {
        for message in Self::bounded_transcript_messages(messages) {
            let payload = serde_json::to_string(&message)
                .context("Failed to serialize ACP turn checkpoint event")?;
            tx.execute(
                "INSERT INTO acp_turn_checkpoint_events (session_id, payload) VALUES (?1, ?2)",
                params![session_id, payload],
            )
            .context("Failed to append ACP turn checkpoint event")?;
        }
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
        let range_kind = TerminalRangeKind::for_terminal_batch(&messages);
        Self::append_messages(&tx, session_uuid, session_id, &messages, &now, range_kind)?;
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
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Failed to begin ACP turn checkpoint recovery")?;
        let session_id = tx
            .query_row(
                "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
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
                "SELECT payload FROM acp_turn_checkpoint_events
                 WHERE session_id = ?1 ORDER BY id ASC",
            )
            .context("Failed to prepare ACP turn checkpoint event read")?;
        let payloads = statement
            .query_map(params![session_id], |row| row.get::<_, String>(0))
            .context("Failed to read ACP turn checkpoint events")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to collect ACP turn checkpoint events")?;
        drop(statement);
        let fragments = payloads
            .into_iter()
            .map(|payload| {
                serde_json::from_str::<ConversationMessage>(&payload)
                    .context("Failed to deserialize ACP turn checkpoint event")
            })
            .collect::<Result<Vec<_>>>()?;
        let mut messages =
            Self::bounded_transcript_messages(&Self::fold_checkpoint_fragments(fragments));
        messages.push(ConversationMessage::Chat(ChatMessage::system(
            interruption_marker,
        )));
        // A recovered turn's rows settle in one transaction, but the turn
        // itself was interrupted: the range records that so compaction never
        // certifies it as completed coverage.
        Self::append_messages(
            &tx,
            session_uuid,
            session_id,
            &messages,
            &now,
            TerminalRangeKind::Interrupted,
        )?;
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
    /// result is kept for each unambiguous call id. Duplicate call IDs within
    /// a batch are rejected; recovery markers stay transcript-only.
    pub fn provider_safe_history(messages: &[ConversationMessage]) -> Vec<ConversationMessage> {
        let mut repaired = Vec::new();
        let mut index = 0;
        while index < messages.len() {
            match &messages[index] {
                ConversationMessage::Chat(chat) if chat.role == "system" => {
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
                            if let Some(result) =
                                results.iter().find(|result| result.tool_call_id == call.id)
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
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.session_uuid,
                        s.agent_alias,
                        s.workspace_dir,
                        s.token_count,
                        s.created_at,
                        s.last_activity,
                        (SELECT COUNT(*) FROM acp_messages m WHERE m.session_id = s.id) AS message_count
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
            ) = row.context("Failed to read ACP session row")?;
            out.push(AcpSessionSummary {
                created_at: parse_ts(&created_s, "created_at", &session_uuid),
                last_activity: parse_ts(&activity_s, "last_activity", &session_uuid),
                session_uuid,
                agent_alias,
                workspace_dir,
                token_count: token_count.max(0) as u64,
                message_count: msg_count.max(0) as usize,
            });
        }
        Ok(out)
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
                  WHERE session_uuid = ?2 AND killed_at IS NULL",
                params![now, session_uuid],
            )
            .context("Failed to mark ACP session killed")?;
        let transition = if rows == 1 {
            AcpSessionKillTransition::Marked
        } else {
            let killed_at: Option<Option<String>> = tx
                .query_row(
                    "SELECT killed_at FROM acp_sessions WHERE session_uuid = ?1",
                    params![session_uuid],
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

    // ── manual context compaction ─────────────────────────────────

    fn active_checkpoint_row(
        conn: &Connection,
        session_id: i64,
    ) -> Result<Option<AcpActiveCheckpointRecord>> {
        conn.query_row(
            "SELECT format_version, operation_id, source_first_message_id,
                    covered_through_message_id, source_message_rows, summary,
                    summary_model_provider, summary_model, input_tokens, output_tokens, created_at
             FROM acp_compaction_checkpoints
             WHERE session_id = ?1 AND active = 1
             ORDER BY id DESC LIMIT 1",
            params![session_id],
            |row| {
                Ok(AcpActiveCheckpointRecord {
                    format_version: row.get(0)?,
                    operation_id: row.get(1)?,
                    source_first_message_id: row.get(2)?,
                    covered_through_message_id: row.get(3)?,
                    source_message_rows: row.get(4)?,
                    summary: row.get(5)?,
                    summary_model_provider: row.get(6)?,
                    summary_model: row.get(7)?,
                    input_tokens: row.get::<_, Option<i64>>(8)?.map(|v| v.max(0) as u64),
                    output_tokens: row.get::<_, Option<i64>>(9)?.map(|v| v.max(0) as u64),
                    created_at: row.get(10)?,
                })
            },
        )
        .optional()
        .context("Failed to read active ACP compaction checkpoint")
    }

    fn terminal_range_rows(conn: &Connection, session_id: i64) -> Result<Vec<AcpTerminalRangeRow>> {
        let mut stmt = conn
            .prepare(
                "SELECT first_message_id, last_message_id, terminal_kind
                 FROM acp_terminal_ranges
                 WHERE session_id = ?1
                 ORDER BY first_message_id ASC",
            )
            .context("Failed to prepare terminal-range query")?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("Failed to read terminal ranges")?;
        let mut out = Vec::new();
        for row in rows {
            let (first, last, kind) = row.context("Failed to read terminal-range row")?;
            out.push(AcpTerminalRangeRow {
                first_message_id: first,
                last_message_id: last,
                kind: TerminalRangeKind::from_persisted(&kind)?,
            });
        }
        Ok(out)
    }

    /// Read the full compaction state for one operation in a single SQLite
    /// read snapshot (one read transaction — not just the process mutex, so
    /// a concurrent writer in another process cannot split the view).
    /// Returns `None` when the session row does not exist.
    pub fn read_compaction_snapshot(
        &self,
        session_uuid: &str,
        operation_id: &str,
    ) -> Result<Option<AcpCompactionSnapshot>> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .context("Failed to begin compaction snapshot read")?;

        let row = tx
            .query_row(
                "SELECT id, agent_alias, workspace_dir, interaction_surface, killed_at
                 FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
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
            .optional()
            .context("Failed to query ACP session for compaction")?;
        let Some((session_row_id, agent_alias, workspace_dir, interaction_surface, killed_at)) =
            row
        else {
            return Ok(None);
        };

        let message_rows = Self::load_message_rows(&tx, session_row_id)?;
        let terminal_ranges = Self::terminal_range_rows(&tx, session_row_id)?;
        let active_checkpoint = Self::active_checkpoint_row(&tx, session_row_id)?;
        let operation_checkpoint: Option<i64> = tx
            .query_row(
                "SELECT active FROM acp_compaction_checkpoints
                 WHERE session_id = ?1 AND operation_id = ?2
                 ORDER BY id DESC LIMIT 1",
                params![session_row_id, operation_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read operation compaction checkpoint")?;
        let inflight_turn_id: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_row_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read in-flight ACP turn checkpoint")?;
        tx.commit()
            .context("Failed to close compaction snapshot read")?;

        Ok(Some(AcpCompactionSnapshot {
            session_row_id,
            session_uuid: session_uuid.to_string(),
            agent_alias,
            workspace_dir,
            interaction_surface,
            killed: killed_at.is_some(),
            message_rows,
            terminal_ranges,
            active_checkpoint,
            operation_checkpoint: operation_checkpoint.map(|active| {
                if active == 1 {
                    AcpCheckpointOperationState::Active
                } else {
                    AcpCheckpointOperationState::Inactive
                }
            }),
            inflight_turn_id,
        }))
    }

    /// Restore-mode load pairing durable originals with the active
    /// compaction checkpoint in one read snapshot. Killed rows stay terminal
    /// for runtime restore, exactly like [`Self::load_session_for_restore`].
    pub fn load_session_for_restore_with_projection(
        &self,
        session_uuid: &str,
    ) -> Result<AcpSessionRestoreProjection> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .context("Failed to begin projected restore read")?;

        let row = tx
            .query_row(
                "SELECT id, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity, killed_at
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
                    ))
                },
            )
            .optional()
            .context("Failed to query ACP session for projected restore")?;
        let Some((
            session_id,
            agent_alias,
            workspace_dir,
            interaction_surface,
            token_count,
            created_at_s,
            last_activity_s,
            killed_at,
        )) = row
        else {
            return Ok(AcpSessionRestoreProjection::Missing);
        };
        if killed_at.is_some() {
            return Ok(AcpSessionRestoreProjection::Killed);
        }

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);
        let message_rows = Self::load_message_rows(&tx, session_id)?;
        let checkpoint = Self::active_checkpoint_row(&tx, session_id)?;
        tx.commit()
            .context("Failed to close projected restore read")?;

        Ok(AcpSessionRestoreProjection::Projected(Box::new(
            AcpProjectedRestore {
                data: AcpSessionData {
                    session_uuid: session_uuid.to_string(),
                    agent_alias,
                    workspace_dir,
                    interaction_surface,
                    token_count: token_count.max(0) as u64,
                    created_at,
                    last_activity,
                    messages: message_rows
                        .iter()
                        .map(|(_, message)| message.clone())
                        .collect(),
                },
                message_rows,
                checkpoint,
            },
        )))
    }

    /// Validate that the terminal ranges exactly tile the span
    /// [first, covered_through] with only completed turns, and that the
    /// message-row count still matches the caller's snapshot. Returns a
    /// human-readable detail on mismatch.
    fn validate_source_identity(
        conn: &Connection,
        session_id: i64,
        first_message_id: i64,
        covered_through_message_id: i64,
        expected_message_rows: i64,
    ) -> std::result::Result<(), String> {
        // Adjacency is SESSION-LOCAL: acp_messages ids are global, so
        // another session's intervening rows create harmless numeric gaps
        // between this session's ranges. Coverage is checked against this
        // session's own ordered rows instead of `last_id + 1` contiguity.
        let mut stmt = conn
            .prepare(
                "SELECT id FROM acp_messages
                 WHERE session_id = ?1 AND id >= ?2 AND id <= ?3
                 ORDER BY id ASC",
            )
            .map_err(|error| format!("failed to read covered rows: {error}"))?;
        let row_ids: Vec<i64> = stmt
            .query_map(
                params![session_id, first_message_id, covered_through_message_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("failed to read covered rows: {error}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("failed to read covered rows: {error}"))?;
        if row_ids.len() as i64 != expected_message_rows {
            return Err(format!(
                "covered row count changed: expected {expected_message_rows}, found {}",
                row_ids.len()
            ));
        }

        let ranges = Self::terminal_range_rows(conn, session_id)
            .map_err(|error| format!("failed to read terminal ranges: {error}"))?;
        // Only ranges that intersect the covered span participate; any range
        // straddling the boundary proves the boundary is not a range end.
        let mut covering: Vec<&AcpTerminalRangeRow> = ranges
            .iter()
            .filter(|range| {
                range.first_message_id <= covered_through_message_id
                    && range.last_message_id >= first_message_id
            })
            .collect();
        covering.sort_by_key(|range| range.first_message_id);
        if let Some(last) = covering.last()
            && last.last_message_id > covered_through_message_id
        {
            return Err(format!(
                "covered boundary {covered_through_message_id} is not a terminal \
                 range end (range {first}..{last} straddles it)",
                first = last.first_message_id,
                last = last.last_message_id
            ));
        }

        let row_position: std::collections::HashMap<i64, usize> = row_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        let mut next_row = 0usize;
        for range in covering {
            if range.kind != TerminalRangeKind::Completed {
                return Err(format!(
                    "range starting at row {first} settled as {kind}, not completed",
                    first = range.first_message_id,
                    kind = range.kind.as_str()
                ));
            }
            // The range must start exactly at this session's next uncovered
            // row and end on one of this session's later rows; anything
            // else is a genuine gap or overlap inside the covered span.
            let starts_at_next = row_ids
                .get(next_row)
                .is_some_and(|id| *id == range.first_message_id);
            let ends_in_session = row_position
                .get(&range.last_message_id)
                .is_some_and(|end| *end >= next_row);
            if !starts_at_next || !ends_in_session {
                return Err(format!(
                    "terminal ranges do not tile this session's rows \
                     [row {first}..{covered_through_message_id}] without gaps",
                    first = first_message_id
                ));
            }
            next_row = row_position[&range.last_message_id] + 1;
        }
        if next_row != row_ids.len() {
            return Err(format!(
                "terminal ranges do not tile this session's rows \
                 [row {first}..{covered_through_message_id}] without gaps",
                first = first_message_id
            ));
        }
        Ok(())
    }

    /// Activate (or idempotently recognize) one compaction checkpoint in a
    /// single write transaction that rechecks the durable world: session
    /// incarnation, not-killed state, absence of a durable in-flight turn,
    /// and exact source identity. A pre-existing active checkpoint for a
    /// different operation is deactivated (recompaction replaces the old
    /// projection with one recomputed from originals); the same operation id
    /// is recognized as a committed retry without a second write.
    pub fn activate_compaction_checkpoint(
        &self,
        request: &CompactionActivationRequest<'_>,
    ) -> std::result::Result<CompactionActivationOutcome, CompactionActivationError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| CompactionActivationError::Storage(error.to_string()))?;

        let row = tx
            .query_row(
                "SELECT id, killed_at FROM acp_sessions WHERE session_uuid = ?1",
                params![request.session_uuid],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|error| CompactionActivationError::Storage(error.to_string()))?;
        let Some((session_row_id, killed_at)) = row else {
            return Err(CompactionActivationError::SessionMissing);
        };
        if session_row_id != request.expected_session_row_id {
            return Err(CompactionActivationError::IncarnationMismatch {
                found_session_row_id: session_row_id,
            });
        }
        if killed_at.is_some() {
            return Err(CompactionActivationError::SessionKilled);
        }
        let inflight: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_row_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| CompactionActivationError::Storage(error.to_string()))?;
        if inflight.is_some() {
            return Err(CompactionActivationError::InflightTurn);
        }

        if let Err(detail) = Self::validate_source_identity(
            &tx,
            session_row_id,
            request.source_first_message_id,
            request.covered_through_message_id,
            request.source_message_rows,
        ) {
            return Err(CompactionActivationError::SourceMismatch { detail });
        }

        // Committed retry: the same operation is already active.
        let active_operation: Option<String> = tx
            .query_row(
                "SELECT operation_id FROM acp_compaction_checkpoints
                 WHERE session_id = ?1 AND active = 1",
                params![session_row_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| CompactionActivationError::Storage(error.to_string()))?;
        if active_operation.as_deref() == Some(request.operation_id) {
            return Ok(CompactionActivationOutcome::AlreadyActive);
        }

        // Fence on the caller's snapshotted prior active operation: only a
        // request that saw THIS active checkpoint (or correctly saw none)
        // may supersede it. A stale request must never overwrite a later
        // compaction's committed projection.
        if active_operation.as_deref() != request.expected_prior_active_operation {
            return Err(CompactionActivationError::StaleActiveCheckpoint {
                active_operation,
                expected_operation: request.expected_prior_active_operation.map(str::to_string),
            });
        }

        // A different active checkpoint is superseded: recompaction replaces
        // the old projection with one recomputed from originals. Only an
        // actual restore records deactivated_by_operation for restore retries.
        tx.execute(
            "UPDATE acp_compaction_checkpoints
             SET active = 0, deactivated_at = ?2, deactivated_by_operation = NULL
             WHERE session_id = ?1 AND active = 1",
            params![session_row_id, now],
        )
        .map_err(|error| CompactionActivationError::Storage(error.to_string()))?;
        tx.execute(
            "INSERT INTO acp_compaction_checkpoints
               (session_id, format_version, operation_id, source_first_message_id,
                covered_through_message_id, source_message_rows, summary,
                summary_model_provider, summary_model, input_tokens, output_tokens,
                created_at, active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1)",
            params![
                session_row_id,
                request.format_version,
                request.operation_id,
                request.source_first_message_id,
                request.covered_through_message_id,
                request.source_message_rows,
                request.summary,
                request.summary_model_provider,
                request.summary_model,
                request.input_tokens.map(|v| v as i64),
                request.output_tokens.map(|v| v as i64),
                now,
            ],
        )
        .map_err(|error| CompactionActivationError::Storage(error.to_string()))?;
        tx.commit()
            .map_err(|error| CompactionActivationError::Storage(error.to_string()))?;
        Ok(CompactionActivationOutcome::Activated)
    }

    /// Deactivate the active compaction checkpoint (restore) in one write
    /// transaction rechecking incarnation, kill state, in-flight turns and
    /// the identity of the checkpoint the caller snapshotted. Idempotent
    /// per operation id: a retry of the same restore is recognized, a
    /// restore of a session with no active checkpoint is a typed no-op, and
    /// a stale request whose snapshotted checkpoint was replaced by a later
    /// operation is refused instead of deactivating the successor. Restore
    /// deactivates the checkpoint — it never deletes history, reruns tools,
    /// or reverses external effects.
    #[allow(clippy::too_many_arguments)]
    pub fn deactivate_compaction_checkpoint(
        &self,
        session_uuid: &str,
        expected_session_row_id: i64,
        operation_id: &str,
        expected_active_checkpoint: Option<(&str, i64)>,
    ) -> std::result::Result<CompactionDeactivationOutcome, CompactionDeactivationError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| CompactionDeactivationError::Storage(error.to_string()))?;

        let row = tx
            .query_row(
                "SELECT id, killed_at FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|error| CompactionDeactivationError::Storage(error.to_string()))?;
        let Some((session_row_id, killed_at)) = row else {
            return Err(CompactionDeactivationError::SessionMissing);
        };
        if session_row_id != expected_session_row_id {
            return Err(CompactionDeactivationError::IncarnationMismatch {
                found_session_row_id: session_row_id,
            });
        }
        if killed_at.is_some() {
            return Err(CompactionDeactivationError::SessionKilled);
        }
        let inflight: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM acp_turn_checkpoints WHERE session_id = ?1",
                params![session_row_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| CompactionDeactivationError::Storage(error.to_string()))?;
        if inflight.is_some() {
            return Err(CompactionDeactivationError::InflightTurn);
        }

        // Recognize this restore before inspecting the current checkpoint:
        // an old retry must not deactivate a newer compaction.
        let already_restored: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM acp_compaction_checkpoints
                 WHERE session_id = ?1 AND deactivated_by_operation = ?2)",
                params![session_row_id, operation_id],
                |row| row.get(0),
            )
            .map_err(|error| CompactionDeactivationError::Storage(error.to_string()))?;
        if already_restored {
            return Ok(CompactionDeactivationOutcome::AlreadyDeactivated);
        }

        let active: Option<(String, i64, i64, i64)> = tx
            .query_row(
                "SELECT operation_id, covered_through_message_id, source_message_rows, id
                 FROM acp_compaction_checkpoints
                 WHERE session_id = ?1 AND active = 1",
                params![session_row_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| CompactionDeactivationError::Storage(error.to_string()))?;
        let Some((active_operation, covered_through_message_id, source_message_rows, _row_id)) =
            active
        else {
            return Ok(CompactionDeactivationOutcome::NoActiveCheckpoint);
        };
        // Fence on the caller's snapshotted checkpoint identity: a stale
        // restore must not deactivate a checkpoint that a later compact or
        // recompaction installed after this request's snapshot.
        let expected =
            expected_active_checkpoint.map(|(operation, covered)| (operation.to_string(), covered));
        if expected.as_ref() != Some(&(active_operation.clone(), covered_through_message_id)) {
            return Err(CompactionDeactivationError::StaleActiveCheckpoint {
                active_operation: Some(active_operation),
                expected_operation: expected.map(|(operation, _)| operation),
            });
        }
        let changed = tx
            .execute(
                "UPDATE acp_compaction_checkpoints
                 SET active = 0, deactivated_at = ?2, deactivated_by_operation = ?3
                 WHERE session_id = ?1 AND active = 1",
                params![session_row_id, now, operation_id],
            )
            .map_err(|error| CompactionDeactivationError::Storage(error.to_string()))?;
        if changed != 1 {
            return Err(CompactionDeactivationError::Storage(format!(
                "deactivation changed {changed} rows instead of 1"
            )));
        }
        tx.commit()
            .map_err(|error| CompactionDeactivationError::Storage(error.to_string()))?;
        Ok(CompactionDeactivationOutcome::Deactivated {
            covered_through_message_id,
            covered_message_rows: source_message_rows,
        })
    }
}

/// Select the contiguous known-completed prefix that a manual compaction may
/// cover, from the session's original rows and settled terminal ranges.
///
/// Coverage rules (v1, all fail-closed):
/// - Coverage starts at the session's first message row and is certified by
///   terminal ranges only. Legacy or unknown rows are refused — never
///   inferred from user-message boundaries, token estimates, or audit rows.
/// - Only `Completed` ranges may be covered. `Failed` and `Interrupted`
///   ranges stay raw tail history rather than being certified complete.
/// - The newest completed turn is always retained: coverage is truncated
///   before it, and everything after it stays intact.
/// - Every tool exchange in the covered span must pair unambiguously
///   one-to-one with its immediately adjacent results, mirroring the
///   replay filter's pairing contract but refusing instead of repairing.
pub fn select_compaction_source(
    message_rows: &[(i64, ConversationMessage)],
    ranges: &[AcpTerminalRangeRow],
) -> std::result::Result<CompactionSourceSelection, CompactionSourceError> {
    let first_message_id = match message_rows.first() {
        Some((id, _)) => *id,
        None => {
            return Err(CompactionSourceError::NoTerminalCoverage {
                first_message_id: 0,
            });
        }
    };

    // Coverage must start with a completed range at the head row.
    let head = ranges
        .first()
        .ok_or(CompactionSourceError::NoTerminalCoverage { first_message_id })?;
    if head.first_message_id != first_message_id {
        return Err(CompactionSourceError::NoTerminalCoverage { first_message_id });
    }
    if head.kind != TerminalRangeKind::Completed {
        return Err(CompactionSourceError::LeadingRangeNotCompleted {
            kind: head.kind,
            first_message_id: head.first_message_id,
        });
    }

    // The newest completed turn is always retained; coverage may only use
    // ranges strictly before it. Range order is row order, so every range
    // before that index ends before the newest completed turn begins.
    let Some(newest_completed) = ranges
        .iter()
        .rposition(|range| range.kind == TerminalRangeKind::Completed)
    else {
        return Err(CompactionSourceError::NewestTurnMustBeRetained);
    };

    // Adjacency is SESSION-LOCAL: acp_messages ids are global, so another
    // session's intervening rows create harmless numeric gaps between this
    // session's ranges. A range continues the covered prefix only when it
    // starts at THIS session's next uncovered row and ends on one of this
    // session's later rows. Genuinely missing coverage of this session's
    // own rows still stops the walk.
    let row_position: std::collections::HashMap<i64, usize> = message_rows
        .iter()
        .enumerate()
        .map(|(index, (id, _))| (*id, index))
        .collect();
    let mut next_row = 0usize;
    let mut selected_ranges = 0usize;
    for (index, range) in ranges.iter().enumerate() {
        if index >= newest_completed {
            break;
        }
        if range.kind != TerminalRangeKind::Completed {
            break;
        }
        let starts_at_next = message_rows
            .get(next_row)
            .is_some_and(|(id, _)| *id == range.first_message_id);
        let ends_in_session = row_position
            .get(&range.last_message_id)
            .is_some_and(|end| *end >= next_row);
        if !starts_at_next || !ends_in_session {
            break;
        }
        next_row = row_position[&range.last_message_id] + 1;
        selected_ranges += 1;
    }
    if selected_ranges == 0 {
        return Err(CompactionSourceError::NewestTurnMustBeRetained);
    }
    let covered_row_count = next_row;
    let covered_through_message_id = message_rows[next_row - 1].0;

    let covered: Vec<(i64, &ConversationMessage)> = message_rows
        .iter()
        .take_while(|(id, _)| *id <= covered_through_message_id)
        .map(|(id, message)| (*id, message))
        .collect();
    // `covered_row_count` counts distinct message rows (the walk's
    // positions); `covered` may hold more entries when one row decomposes
    // into an AssistantToolCalls plus a ToolResults message.
    let covered_message_rows = covered_row_count;

    validate_covered_tool_pairing(&covered)?;

    Ok(CompactionSourceSelection {
        first_message_id,
        covered_through_message_id,
        covered_message_rows,
        covered_ranges: selected_ranges,
    })
}

/// Refuse ambiguous or unpaired tool exchanges inside the covered span.
/// Every `AssistantToolCalls` must be immediately followed by the
/// `ToolResults` message that resolves each call exactly once, with no
/// duplicate call ids, no orphan results, and no dangling calls.
fn validate_covered_tool_pairing(
    covered: &[(i64, &ConversationMessage)],
) -> std::result::Result<(), CompactionSourceError> {
    let mut index = 0usize;
    while index < covered.len() {
        let (message_id, message) = covered[index];
        match message {
            ConversationMessage::Chat(_) => {
                index += 1;
            }
            ConversationMessage::AssistantToolCalls { tool_calls, .. } => {
                let Some((_, ConversationMessage::ToolResults(results))) = covered.get(index + 1)
                else {
                    return Err(CompactionSourceError::AmbiguousToolPairing { message_id });
                };
                let mut call_ids = std::collections::HashSet::new();
                for call in tool_calls {
                    if !call_ids.insert(call.id.as_str()) {
                        return Err(CompactionSourceError::AmbiguousToolPairing { message_id });
                    }
                }
                let mut result_ids = std::collections::HashSet::new();
                for result in results {
                    if !result_ids.insert(result.tool_call_id.as_str()) {
                        return Err(CompactionSourceError::AmbiguousToolPairing { message_id });
                    }
                }
                if call_ids.len() != results.len() || call_ids != result_ids {
                    return Err(CompactionSourceError::AmbiguousToolPairing { message_id });
                }
                index += 2;
            }
            // A results message reached as a walk position has no preceding
            // call inside the covered span: an orphan result.
            ConversationMessage::ToolResults(_) => {
                return Err(CompactionSourceError::AmbiguousToolPairing { message_id });
            }
        }
    }
    Ok(())
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
    use zeroclaw_api::model_provider::{ChatMessage, ToolCall, ToolResultMessage};

    fn open_store() -> (TempDir, AcpSessionStore) {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        (tmp, store)
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
    fn create_and_load_session_metadata() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-abc", "personal_code", "/home/user/project")
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
            .create_session("sess-legacy", "alpha", "/tmp/proj")
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
            .create_session("sess-plan", "alpha", "/tmp/proj")
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
            .create_session("sess-msgs", "alpha", "/tmp/proj")
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

    #[test]
    fn interrupted_checkpoint_recovers_once_with_marker() {
        let (_tmp, store) = open_store();
        store
            .create_session("checkpoint-session", "default", "/tmp/workspace")
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
                .create_session(&session_id, "default", "/tmp/workspace")
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
            .create_session("checkpoint-identity", "default", "/tmp/workspace")
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
            .create_session("checkpoint-finalize", "default", "/tmp/workspace")
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
            .create_session("checkpoint-killed", "default", "/tmp/workspace")
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
            ConversationMessage::ToolResults(vec![
                ToolResultMessage {
                    tool_call_id: "paired".to_string(),
                    tool_name: "shell".to_string(),
                    content: "first".to_string(),
                },
                ToolResultMessage {
                    tool_call_id: "paired".to_string(),
                    tool_name: "shell".to_string(),
                    content: "duplicate".to_string(),
                },
            ]),
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
    fn provider_safe_history_rejects_duplicate_call_ids_per_batch() {
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
        for result_count in [1, 2] {
            for keep_unique_peer in [false, true] {
                let mut calls = vec![call("duplicate"), call("duplicate")];
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
                assert_eq!(
                    serde_json::to_value(AcpSessionStore::provider_safe_history(&messages))
                        .unwrap(),
                    serde_json::to_value(expected).unwrap(),
                    "duplicate result count={result_count}, unique peer={keep_unique_peer}"
                );
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
            .create_session("bounded-terminal", "default", "/tmp/workspace")
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
    fn append_turn_decomposes_assistant_tool_calls_and_results() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-variants", "alpha", "/tmp/proj")
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
    fn append_turn_round_trips_failed_turn_system_marker_row() {
        // The durable shape a failed turn leaves behind: accepted prompt,
        // one complete tool exchange, then the fixed `system` marker row.
        // load_messages must return them verbatim — the provider-replay
        // exclusion of system rows happens at seed time (runtime side), not
        // here; the transcript reads the marker.
        let (_tmp, store) = open_store();
        store
            .create_session("sess-failed", "alpha", "/tmp/proj")
            .unwrap();

        let msgs = vec![
            ConversationMessage::Chat(ChatMessage::user("write the file")),
            ConversationMessage::AssistantToolCalls {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "tc-1".into(),
                    name: "shell".into(),
                    arguments: r#"{"command":"ls"}"#.into(),
                    extra_content: None,
                }],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc-1".into(),
                content: "ok".into(),
                tool_name: "shell".into(),
            }]),
            ConversationMessage::Chat(ChatMessage::system(FAILED_TURN_MARKER)),
        ];
        store.append_turn("sess-failed", &msgs).unwrap();

        let data = store.load_session("sess-failed").unwrap().unwrap();
        assert_eq!(data.messages.len(), 4);
        assert!(matches!(
            &data.messages[3],
            ConversationMessage::Chat(m)
                if m.role == "system" && m.content == FAILED_TURN_MARKER
        ));
    }

    #[test]
    fn no_data_duplication_tool_call_payload_only_in_tool_calls_table() {
        // The schema contract: tool-call args and results live ONLY in
        // acp_tool_calls. The assistant's message row carries only the text.
        let (_tmp, store) = open_store();
        store
            .create_session("sess-dup", "alpha", "/tmp/proj")
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
            .create_session("sess-empty", "alpha", "/tmp/proj")
            .unwrap();
        store.append_turn("sess-empty", &[]).unwrap();
        let data = store.load_session("sess-empty").unwrap().unwrap();
        assert!(data.messages.is_empty());
    }

    #[test]
    fn last_activity_updated_on_append() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-activity", "alpha", "/tmp/proj")
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
    fn delete_session_cascades_to_children() {
        let (_tmp, store) = open_store();
        store
            .create_session("sess-del", "alpha", "/tmp/proj")
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
    fn mark_session_killed_persists_without_deleting_history() {
        let (tmp, store) = open_store();
        store
            .create_session("sess-kill", "alpha", "/tmp/proj")
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
            .create_session("sess-atomic-kill", "alpha", "/tmp/proj")
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
            .create_session("sess-touch", "alpha", "/tmp/proj")
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
            .create_session("sess-tok", "alpha", "/tmp/proj")
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
            .create_session("sess-clr", "alpha", "/tmp/proj")
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
            .create_session("sess-seq", "alpha", "/tmp/proj")
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
            .create_session("sess-rej", "alpha", "/tmp/proj")
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
            .create_session("sess-evt", "alpha", "/tmp/proj")
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
        store.create_session("sess-old", "alpha", "/tmp/a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.create_session("sess-new", "beta", "/tmp/b").unwrap();
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
            .create_session("sess-live", "alpha", "/tmp/live")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .create_session("sess-killed", "alpha", "/tmp/killed")
            .unwrap();
        store.mark_session_killed("sess-killed").unwrap();

        let list = store.list_sessions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_uuid, "sess-live");
    }

    #[test]
    fn list_live_sessions_by_agent_filters_owner_and_killed_rows() {
        let (_tmp, store) = open_store();
        store
            .create_session("alpha-old", "alpha", "/ws/old")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .create_session("alpha-new", "alpha", "/ws/new")
            .unwrap();
        store
            .append_turn(
                "alpha-new",
                &[ConversationMessage::Chat(ChatMessage::user("hello"))],
            )
            .unwrap();
        store
            .create_session("alpha-killed", "alpha", "/ws/killed")
            .unwrap();
        store.mark_session_killed("alpha-killed").unwrap();
        store
            .create_session("beta-live", "beta", "/ws/beta")
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
        store.create_session("a-live", "alpha", "/ws/a1").unwrap();
        store.create_session("a-killed", "alpha", "/ws/a2").unwrap();
        store.mark_session_killed("a-killed").unwrap();
        store.create_session("b-live", "beta", "/ws/b1").unwrap();

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
        store.create_session("a-live", "alpha", "/ws/a1").unwrap();
        store.create_session("a-killed", "alpha", "/ws/a2").unwrap();
        store.mark_session_killed("a-killed").unwrap();
        store.create_session("b-live", "beta", "/ws/b1").unwrap();

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

    // ── manual context compaction ─────────────────────────────────

    fn compaction_turn(store: &AcpSessionStore, session: &str, turn: &str) {
        store
            .append_turn(
                session,
                &[
                    ConversationMessage::Chat(ChatMessage::user(turn)),
                    ConversationMessage::Chat(ChatMessage::assistant(format!("answer to {turn}"))),
                ],
            )
            .unwrap();
    }

    fn failed_turn(store: &AcpSessionStore, session: &str, turn: &str) {
        store
            .append_turn(
                session,
                &[
                    ConversationMessage::Chat(ChatMessage::user(turn)),
                    ConversationMessage::Chat(ChatMessage::system(FAILED_TURN_MARKER)),
                ],
            )
            .unwrap();
    }

    fn projected_for_test(restore: AcpSessionRestoreProjection) -> AcpProjectedRestore {
        match restore {
            AcpSessionRestoreProjection::Projected(projected) => *projected,
            AcpSessionRestoreProjection::Missing | AcpSessionRestoreProjection::Killed => {
                panic!("expected projected restore, got Missing or Killed")
            }
        }
    }

    fn rows_and_ranges(
        store: &AcpSessionStore,
        session: &str,
    ) -> (Vec<(i64, ConversationMessage)>, Vec<AcpTerminalRangeRow>) {
        let snapshot = store.read_compaction_snapshot(session, "op-probe").unwrap();
        let snapshot = snapshot.expect("session row must exist");
        (snapshot.message_rows, snapshot.terminal_ranges)
    }

    #[test]
    fn terminal_ranges_classify_completed_failed_and_interrupted() {
        let (_tmp, store) = open_store();
        store.create_session("ranges", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "ranges", "one");
        failed_turn(&store, "ranges", "two");

        // Interrupted: an in-flight checkpoint recovered with the marker.
        store
            .begin_turn_checkpoint(
                "ranges",
                "turn-3",
                &[ConversationMessage::Chat(ChatMessage::user("three"))],
            )
            .unwrap();
        assert!(
            store
                .recover_turn_checkpoint("ranges", "stream interrupted")
                .unwrap()
        );

        let (_, ranges) = rows_and_ranges(&store, "ranges");
        assert_eq!(
            ranges.iter().map(|range| range.kind).collect::<Vec<_>>(),
            vec![
                TerminalRangeKind::Completed,
                TerminalRangeKind::Failed,
                TerminalRangeKind::Interrupted,
            ]
        );
        // Ranges tile the rows in order with no gaps.
        let mut expected_next = ranges[0].first_message_id;
        for range in &ranges {
            assert_eq!(range.first_message_id, expected_next);
            expected_next = range.last_message_id + 1;
        }
    }

    #[test]
    fn select_source_covers_completed_prefix_and_retains_newest_completed() {
        let (_tmp, store) = open_store();
        store.create_session("select", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "select", "one");
        compaction_turn(&store, "select", "two");
        compaction_turn(&store, "select", "three");
        let (rows, ranges) = rows_and_ranges(&store, "select");

        let selection = select_compaction_source(&rows, &ranges).unwrap();
        // Turn three (the newest completed) is retained; turns one and two
        // are covered.
        assert_eq!(selection.covered_ranges, 2);
        assert_eq!(selection.first_message_id, rows[0].0);
        assert_eq!(
            selection.covered_through_message_id,
            ranges[1].last_message_id
        );
        assert_eq!(selection.covered_message_rows, 4);
    }

    #[test]
    fn select_source_refuses_legacy_interrupted_and_single_turn_history() {
        // Legacy: rows exist but no terminal ranges certify them.
        let (_tmp, store) = open_store();
        store.create_session("legacy", "alpha", "/tmp/ws").unwrap();
        store
            .append_turn(
                "legacy",
                &[ConversationMessage::Chat(ChatMessage::user("old"))],
            )
            .unwrap();
        let conn = store.conn.lock();
        conn.execute("DELETE FROM acp_terminal_ranges", []).unwrap();
        drop(conn);
        let (rows, ranges) = rows_and_ranges(&store, "legacy");
        assert!(matches!(
            select_compaction_source(&rows, &ranges),
            Err(CompactionSourceError::NoTerminalCoverage { .. })
        ));

        // Interrupted head: the oldest settled range is an interrupted turn.
        let (_tmp, store) = open_store();
        store
            .create_session("interrupted-head", "alpha", "/tmp/ws")
            .unwrap();
        store
            .begin_turn_checkpoint(
                "interrupted-head",
                "turn-1",
                &[ConversationMessage::Chat(ChatMessage::user("q"))],
            )
            .unwrap();
        assert!(
            store
                .recover_turn_checkpoint("interrupted-head", "interrupted")
                .unwrap()
        );
        compaction_turn(&store, "interrupted-head", "later");
        let (rows, ranges) = rows_and_ranges(&store, "interrupted-head");
        assert!(matches!(
            select_compaction_source(&rows, &ranges),
            Err(CompactionSourceError::LeadingRangeNotCompleted {
                kind: TerminalRangeKind::Interrupted,
                ..
            })
        ));

        // Single completed turn: it is the newest, so it must be retained.
        let (_tmp, store) = open_store();
        store.create_session("single", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "single", "only");
        let (rows, ranges) = rows_and_ranges(&store, "single");
        assert_eq!(
            select_compaction_source(&rows, &ranges),
            Err(CompactionSourceError::NewestTurnMustBeRetained)
        );

        // Empty history.
        let (_tmp, store) = open_store();
        store.create_session("empty", "alpha", "/tmp/ws").unwrap();
        let (rows, ranges) = rows_and_ranges(&store, "empty");
        assert!(matches!(
            select_compaction_source(&rows, &ranges),
            Err(CompactionSourceError::NoTerminalCoverage { .. })
        ));
    }

    #[test]
    fn select_source_stops_at_failed_range_and_refuses_ambiguous_pairing() {
        // A failed turn between completed turns halts coverage there.
        let (_tmp, store) = open_store();
        store.create_session("mixed", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "mixed", "one");
        failed_turn(&store, "mixed", "two");
        compaction_turn(&store, "mixed", "three");
        let (rows, ranges) = rows_and_ranges(&store, "mixed");
        let selection = select_compaction_source(&rows, &ranges).unwrap();
        assert_eq!(selection.covered_ranges, 1);
        assert_eq!(
            selection.covered_through_message_id,
            ranges[0].last_message_id
        );

        // Ambiguous pairing: a duplicate call id inside one exchange.
        let covered = vec![
            (1i64, ConversationMessage::Chat(ChatMessage::user("dup"))),
            (
                2,
                ConversationMessage::AssistantToolCalls {
                    text: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "dup".into(),
                            name: "shell".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        },
                        ToolCall {
                            id: "dup".into(),
                            name: "shell".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        },
                    ],
                    reasoning_content: None,
                },
            ),
            (
                2,
                ConversationMessage::ToolResults(vec![
                    ToolResultMessage {
                        tool_call_id: "dup".into(),
                        content: "first".into(),
                        tool_name: "shell".into(),
                    },
                    ToolResultMessage {
                        tool_call_id: "dup".into(),
                        content: "second".into(),
                        tool_name: "shell".into(),
                    },
                ]),
            ),
        ];
        let ranges = vec![
            AcpTerminalRangeRow {
                first_message_id: 1,
                last_message_id: 2,
                kind: TerminalRangeKind::Completed,
            },
            AcpTerminalRangeRow {
                first_message_id: 3,
                last_message_id: 4,
                kind: TerminalRangeKind::Completed,
            },
        ];
        let rows = [
            covered,
            vec![
                (3, ConversationMessage::Chat(ChatMessage::user("tail"))),
                (4, ConversationMessage::Chat(ChatMessage::assistant("done"))),
            ],
        ]
        .concat();
        assert!(matches!(
            select_compaction_source(&rows, &ranges),
            Err(CompactionSourceError::AmbiguousToolPairing { .. })
        ));
    }

    #[test]
    fn source_adjacency_is_session_local_not_global_id_contiguous() {
        let (_tmp, store) = open_store();
        // Two sessions share the global acp_messages id space: session B's
        // rows interleave numerically between session A's turns.
        store.create_session("inter-a", "alpha", "/tmp/ws").unwrap();
        store.create_session("inter-b", "beta", "/tmp/ws").unwrap();
        compaction_turn(&store, "inter-a", "one"); // A rows 1, 2
        compaction_turn(&store, "inter-b", "other"); // B rows 3, 4
        compaction_turn(&store, "inter-a", "two"); // A rows 5, 6
        compaction_turn(&store, "inter-a", "three"); // A rows 7, 8

        // A's coverage ignores B's intervening global ids: the completed
        // ranges (1-2) and (5-6) are adjacent in A's own row list, so the
        // prefix covers both while retaining the newest completed turn.
        let (rows, ranges) = rows_and_ranges(&store, "inter-a");
        let selection = select_compaction_source(&rows, &ranges).unwrap();
        assert_eq!(selection.covered_ranges, 2);
        assert_eq!(selection.covered_message_rows, 4);
        assert_eq!(selection.covered_through_message_id, 6);
        assert_eq!(selection.first_message_id, 1);

        // The store's transactional source validation must agree: the
        // activation of that interleaved coverage commits.
        let snapshot = store
            .read_compaction_snapshot("inter-a", "op-inter-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .activate_compaction_checkpoint(&activation_request(
                    "inter-a",
                    snapshot.session_row_id,
                    "op-inter-1",
                    None,
                    selection
                ))
                .unwrap(),
            CompactionActivationOutcome::Activated
        );
        assert!(
            projected_for_test(
                store
                    .load_session_for_restore_with_projection("inter-a")
                    .unwrap(),
            )
            .checkpoint
            .is_some()
        );

        // Genuine missing coverage is different: when this session's own
        // rows lack a certifying range, the walk stops there instead of
        // treating the rows as covered.
        store.create_session("inter-c", "gamma", "/tmp/ws").unwrap();
        compaction_turn(&store, "inter-c", "one");
        compaction_turn(&store, "inter-c", "two");
        compaction_turn(&store, "inter-c", "three");
        {
            // Remove the middle turn's terminal range: C's rows for turn two
            // become genuinely uncertified.
            let conn = store.conn.lock();
            conn.execute(
                "DELETE FROM acp_terminal_ranges
                 WHERE session_id = (SELECT id FROM acp_sessions WHERE session_uuid = 'inter-c')
                   AND first_message_id = (
                       SELECT first_message_id FROM acp_terminal_ranges
                       WHERE session_id = (SELECT id FROM acp_sessions
                                           WHERE session_uuid = 'inter-c')
                       ORDER BY first_message_id LIMIT 1 OFFSET 1
                   )",
                [],
            )
            .unwrap();
            drop(conn);
        }
        let (rows, ranges) = rows_and_ranges(&store, "inter-c");
        let selection = select_compaction_source(&rows, &ranges).unwrap();
        assert_eq!(
            selection.covered_ranges, 1,
            "the walk must stop at C's genuinely uncertified rows"
        );
        assert_eq!(selection.covered_message_rows, 2);
    }

    #[test]
    fn stale_activation_and_restore_are_fenced_by_prior_active_identity() {
        let (_tmp, store) = open_store();
        store.create_session("fence", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "fence", "one");
        compaction_turn(&store, "fence", "two");
        compaction_turn(&store, "fence", "three");

        let snapshot = store
            .read_compaction_snapshot("fence", "op-late")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();

        // A later compaction commits first.
        store
            .activate_compaction_checkpoint(&activation_request(
                "fence",
                snapshot.session_row_id,
                "op-late",
                None,
                selection,
            ))
            .unwrap();

        // A stale request that snapshotted NO active checkpoint must not
        // supersede the later operation.
        let stale = store
            .activate_compaction_checkpoint(&activation_request(
                "fence",
                snapshot.session_row_id,
                "op-stale",
                None,
                selection,
            ))
            .unwrap_err();
        assert!(matches!(
            stale,
            CompactionActivationError::StaleActiveCheckpoint { .. }
        ));
        let snapshot_after = store
            .read_compaction_snapshot("fence", "op-late")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot_after
                .active_checkpoint
                .as_ref()
                .map(|c| c.operation_id.as_str()),
            Some("op-late"),
            "the stale request must not have superseded the later operation"
        );

        // A stale restore that snapshotted the pre-existing checkpoint's
        // identity must not deactivate a DIFFERENT later checkpoint: replace
        // the active checkpoint with a recompaction, then restore against
        // the old identity.
        let fenced_identity = snapshot_after
            .active_checkpoint
            .map(|c| (c.operation_id.clone(), c.covered_through_message_id))
            .unwrap();
        store
            .deactivate_compaction_checkpoint(
                "fence",
                snapshot.session_row_id,
                "op-restore-prepare",
                Some((&fenced_identity.0, fenced_identity.1)),
            )
            .unwrap();
        let snapshot = store
            .read_compaction_snapshot("fence", "op-recompact")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();
        store
            .activate_compaction_checkpoint(&activation_request(
                "fence",
                snapshot.session_row_id,
                "op-recompact",
                None,
                selection,
            ))
            .unwrap();

        // The stale restore expects the OLD (fenced_identity) checkpoint but
        // the active row is now op-recompact: refused, successor preserved.
        let stale_restore = store
            .deactivate_compaction_checkpoint(
                "fence",
                snapshot.session_row_id,
                "op-restore-stale",
                Some((&fenced_identity.0, fenced_identity.1)),
            )
            .unwrap_err();
        assert!(matches!(
            stale_restore,
            CompactionDeactivationError::StaleActiveCheckpoint { .. }
        ));
        let snapshot_after = store
            .read_compaction_snapshot("fence", "op-recompact")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot_after.active_checkpoint.map(|c| c.operation_id),
            Some("op-recompact".to_string()),
            "the stale restore must not have deactivated the later checkpoint"
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn activation_request<'a>(
        session: &'a str,
        session_row_id: i64,
        operation_id: &'a str,
        expected_prior_active_operation: Option<&'a str>,
        selection: CompactionSourceSelection,
    ) -> CompactionActivationRequest<'a> {
        CompactionActivationRequest {
            session_uuid: session,
            expected_session_row_id: session_row_id,
            format_version: 1,
            operation_id,
            expected_prior_active_operation,
            source_first_message_id: selection.first_message_id,
            covered_through_message_id: selection.covered_through_message_id,
            source_message_rows: selection.covered_message_rows as i64,
            summary: "bounded summary",
            summary_model_provider: "provider",
            summary_model: "model",
            input_tokens: Some(120),
            output_tokens: Some(30),
        }
    }

    #[test]
    fn activate_deactivate_round_trip_keeps_originals_and_one_active_checkpoint() {
        let (_tmp, store) = open_store();
        store.create_session("cycle", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "cycle", "one");
        compaction_turn(&store, "cycle", "two");
        compaction_turn(&store, "cycle", "three");

        let snapshot = store
            .read_compaction_snapshot("cycle", "op-compact-1")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();
        assert_eq!(
            store
                .activate_compaction_checkpoint(&activation_request(
                    "cycle",
                    snapshot.session_row_id,
                    "op-compact-1",
                    None,
                    selection
                ))
                .unwrap(),
            CompactionActivationOutcome::Activated
        );

        // Originals are retained; the legacy reader rejects; the transcript
        // and projected readers stay usable.
        let originals = store.load_session_transcript("cycle").unwrap().unwrap();
        assert_eq!(originals.messages.len(), 6);
        assert!(store.load_session("cycle").is_err());
        let projected = projected_for_test(
            store
                .load_session_for_restore_with_projection("cycle")
                .unwrap(),
        );
        assert_eq!(projected.data.messages.len(), 6);
        assert_eq!(
            projected
                .checkpoint
                .as_ref()
                .map(|c| c.operation_id.as_str()),
            Some("op-compact-1")
        );

        // Later append-only turns extend the tail without invalidating.
        compaction_turn(&store, "cycle", "four");
        assert!(
            projected_for_test(
                store
                    .load_session_for_restore_with_projection("cycle")
                    .unwrap(),
            )
            .checkpoint
            .is_some()
        );

        // Committed retry is recognized without a second write.
        let snapshot = store
            .read_compaction_snapshot("cycle", "op-compact-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.operation_checkpoint,
            Some(AcpCheckpointOperationState::Active)
        );
        assert_eq!(
            store
                .activate_compaction_checkpoint(&activation_request(
                    "cycle",
                    snapshot.session_row_id,
                    "op-compact-1",
                    Some("op-compact-1"),
                    selection
                ))
                .unwrap(),
            CompactionActivationOutcome::AlreadyActive
        );

        // Restore deactivates; the retry of the old compact sees an inactive
        // row (superseded), and a further restore is already-deactivated.
        let active_checkpoint = snapshot.active_checkpoint.clone().unwrap();
        assert!(matches!(
            store
                .deactivate_compaction_checkpoint(
                    "cycle",
                    snapshot.session_row_id,
                    "op-restore-1",
                    Some((
                        active_checkpoint.operation_id.as_str(),
                        active_checkpoint.covered_through_message_id
                    )),
                )
                .unwrap(),
            CompactionDeactivationOutcome::Deactivated { .. }
        ));
        let snapshot = store
            .read_compaction_snapshot("cycle", "op-compact-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.operation_checkpoint,
            Some(AcpCheckpointOperationState::Inactive)
        );
        assert!(snapshot.active_checkpoint.is_none());
        assert_eq!(
            store
                .deactivate_compaction_checkpoint(
                    "cycle",
                    snapshot.session_row_id,
                    "op-restore-1",
                    None,
                )
                .unwrap(),
            CompactionDeactivationOutcome::AlreadyDeactivated
        );
        assert_eq!(
            store
                .deactivate_compaction_checkpoint(
                    "cycle",
                    snapshot.session_row_id,
                    "op-restore-2",
                    None,
                )
                .unwrap(),
            CompactionDeactivationOutcome::NoActiveCheckpoint
        );

        // Recompaction recomputes from originals and replaces: the new
        // checkpoint is active, and only one active row exists.
        let snapshot = store
            .read_compaction_snapshot("cycle", "op-compact-2")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();
        assert!(selection.covered_ranges >= 2);
        assert_eq!(
            store
                .activate_compaction_checkpoint(&activation_request(
                    "cycle",
                    snapshot.session_row_id,
                    "op-compact-2",
                    None,
                    selection
                ))
                .unwrap(),
            CompactionActivationOutcome::Activated
        );
        // A delayed retry of the first restore preserves the newer compact,
        // even when the caller has freshly observed that newer checkpoint.
        assert_eq!(
            store
                .deactivate_compaction_checkpoint(
                    "cycle",
                    snapshot.session_row_id,
                    "op-restore-1",
                    Some(("op-compact-2", selection.covered_through_message_id)),
                )
                .unwrap(),
            CompactionDeactivationOutcome::AlreadyDeactivated
        );
        let conn = store.conn.lock();
        let active: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM acp_compaction_checkpoints WHERE session_id = \
                 (SELECT id FROM acp_sessions WHERE session_uuid = 'cycle') AND active = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(active, 1);
        let snapshot = store
            .read_compaction_snapshot("cycle", "op-compact-2")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.active_checkpoint.map(|c| c.operation_id),
            Some("op-compact-2".to_string())
        );

        // A compact operation that supersedes another checkpoint is not a
        // completed restore, even if the caller later reuses its ID.
        store
            .activate_compaction_checkpoint(&activation_request(
                "cycle",
                snapshot.session_row_id,
                "op-compact-3",
                Some("op-compact-2"),
                selection,
            ))
            .unwrap();
        assert!(matches!(
            store
                .deactivate_compaction_checkpoint(
                    "cycle",
                    snapshot.session_row_id,
                    "op-compact-3",
                    Some(("op-compact-3", selection.covered_through_message_id)),
                )
                .unwrap(),
            CompactionDeactivationOutcome::Deactivated { .. }
        ));
    }

    #[test]
    fn activation_rejects_stale_source_killed_inflight_and_reincarnation() {
        let (_tmp, store) = open_store();
        store
            .create_session("negatives", "alpha", "/tmp/ws")
            .unwrap();
        compaction_turn(&store, "negatives", "one");
        compaction_turn(&store, "negatives", "two");
        compaction_turn(&store, "negatives", "three");
        let snapshot = store
            .read_compaction_snapshot("negatives", "op-1")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();

        // Stale source: boundary not at a terminal range end.
        let mut stale = activation_request(
            "negatives",
            snapshot.session_row_id,
            "op-1",
            None,
            selection,
        );
        stale.covered_through_message_id = selection.covered_through_message_id - 1;
        stale.source_message_rows = selection.covered_message_rows as i64 - 1;
        assert!(matches!(
            store.activate_compaction_checkpoint(&stale).unwrap_err(),
            CompactionActivationError::SourceMismatch { .. }
        ));
        // Nothing was written by the failed attempt.
        let conn = store.conn.lock();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM acp_compaction_checkpoints", [], |r| {
                r.get(0)
            })
            .unwrap();
        drop(conn);
        assert_eq!(rows, 0);

        // In-flight turn checkpoint blocks activation.
        store
            .begin_turn_checkpoint(
                "negatives",
                "turn-live",
                &[ConversationMessage::Chat(ChatMessage::user("running"))],
            )
            .unwrap();
        assert_eq!(
            store
                .activate_compaction_checkpoint(&activation_request(
                    "negatives",
                    snapshot.session_row_id,
                    "op-1",
                    None,
                    selection
                ))
                .unwrap_err(),
            CompactionActivationError::InflightTurn
        );
        assert!(
            store
                .finalize_turn_checkpoint(
                    "negatives",
                    "turn-live",
                    &[
                        ConversationMessage::Chat(ChatMessage::user("running")),
                        ConversationMessage::Chat(ChatMessage::assistant("done")),
                    ],
                )
                .is_ok()
        );

        // Killed sessions refuse activation and restore.
        store.mark_session_killed("negatives").unwrap();
        assert_eq!(
            store
                .activate_compaction_checkpoint(&activation_request(
                    "negatives",
                    snapshot.session_row_id,
                    "op-1",
                    None,
                    selection
                ))
                .unwrap_err(),
            CompactionActivationError::SessionKilled
        );
        assert_eq!(
            store
                .deactivate_compaction_checkpoint(
                    "negatives",
                    snapshot.session_row_id,
                    "op-r",
                    None
                )
                .unwrap_err(),
            CompactionDeactivationError::SessionKilled
        );

        // Delete + recreate: the new incarnation cannot reuse the stale row id.
        let (_tmp, store) = open_store();
        store.create_session("reinc", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "reinc", "one");
        compaction_turn(&store, "reinc", "two");
        let snapshot = store
            .read_compaction_snapshot("reinc", "op-1")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();
        store.delete_session("reinc").unwrap();
        store.create_session("reinc", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "reinc", "one");
        compaction_turn(&store, "reinc", "two");
        assert_eq!(
            store
                .activate_compaction_checkpoint(&activation_request(
                    "reinc",
                    snapshot.session_row_id,
                    "op-1",
                    None,
                    selection
                ))
                .unwrap_err(),
            CompactionActivationError::IncarnationMismatch {
                found_session_row_id: 2
            }
        );
        assert_eq!(
            store
                .deactivate_compaction_checkpoint("reinc", snapshot.session_row_id, "op-r", None)
                .unwrap_err(),
            CompactionDeactivationError::IncarnationMismatch {
                found_session_row_id: 2
            }
        );
    }

    #[test]
    fn delete_session_cascades_to_ranges_and_checkpoints() {
        let (_tmp, store) = open_store();
        store.create_session("cascade", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "cascade", "one");
        compaction_turn(&store, "cascade", "two");
        compaction_turn(&store, "cascade", "three");
        let snapshot = store
            .read_compaction_snapshot("cascade", "op-1")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();
        store
            .activate_compaction_checkpoint(&activation_request(
                "cascade",
                snapshot.session_row_id,
                "op-1",
                None,
                selection,
            ))
            .unwrap();

        assert!(store.delete_session("cascade").unwrap());
        let conn = store.conn.lock();
        for table in ["acp_terminal_ranges", "acp_compaction_checkpoints"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "cascade should empty {table}");
        }
        drop(conn);
        assert!(
            store
                .read_compaction_snapshot("cascade", "op-1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn checkpoint_survives_store_restart_under_synchronous_normal() {
        let (tmp, store) = open_store();
        store.create_session("durable", "alpha", "/tmp/ws").unwrap();
        compaction_turn(&store, "durable", "one");
        compaction_turn(&store, "durable", "two");
        compaction_turn(&store, "durable", "three");
        let snapshot = store
            .read_compaction_snapshot("durable", "op-1")
            .unwrap()
            .unwrap();
        let selection =
            select_compaction_source(&snapshot.message_rows, &snapshot.terminal_ranges).unwrap();
        store
            .activate_compaction_checkpoint(&activation_request(
                "durable",
                snapshot.session_row_id,
                "op-1",
                None,
                selection,
            ))
            .unwrap();

        drop(store);
        let reopened = AcpSessionStore::new(tmp.path()).unwrap();
        let snapshot = reopened
            .read_compaction_snapshot("durable", "op-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.active_checkpoint.map(|c| c.operation_id),
            Some("op-1".to_string())
        );
        assert!(reopened.load_session("durable").is_err());
        assert!(
            projected_for_test(
                reopened
                    .load_session_for_restore_with_projection("durable")
                    .unwrap(),
            )
            .checkpoint
            .is_some()
        );
    }
}
