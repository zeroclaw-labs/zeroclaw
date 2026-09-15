//! ACP session persistence.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage, ToolCall, ToolResultMessage};
use zeroclaw_api::plan::PlanEntry;
use zeroclaw_log::{Action, EventOutcome};

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

/// A bounded, store-owned page of the typed ACP transcript. The cursor is
/// deliberately opaque to RPC clients; callers must hand it back unchanged.
#[derive(Debug)]
pub struct AcpSessionPage {
    pub messages: Vec<ConversationMessage>,
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
             CREATE INDEX IF NOT EXISTS idx_acp_session_events_session ON acp_session_events(session_id, id);",
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

    /// Load session metadata and full message history for restore.
    /// Returns `None` if the session_uuid is not found.
    pub fn load_session(&self, session_uuid: &str) -> Result<Option<AcpSessionData>> {
        let conn = self.conn.lock();

        let row = conn.query_row(
            "SELECT id, agent_alias, workspace_dir, interaction_surface, token_count, created_at, last_activity
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
        ) = match row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e).context("Failed to query ACP session"),
        };

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);

        let messages = Self::load_messages(&conn, session_id)?;

        Ok(Some(AcpSessionData {
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

    /// Load one bounded page of the projected ACP transcript. Cursor reads
    /// hydrate only the durable groups needed for this page; the cursor's
    /// message bound makes later appends invisible to an established walk.
    pub fn load_message_page(
        &self,
        session_uuid: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<AcpSessionPage> {
        let conn = self.conn.lock();
        let session_id: i64 = conn
            .query_row(
                "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow::Error::msg(format!("unknown ACP session: {session_uuid}")))?;
        let snapshot_max: i64 = conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM acp_messages WHERE session_id = ?1",
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
        if state.next_message_id == 0 {
            return Ok(AcpSessionPage {
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
                    "SELECT id, role, content, reasoning_content
                     FROM acp_messages
                     WHERE session_id = ?1 AND id <= ?2 AND id <= ?3
                     ORDER BY id DESC LIMIT 1",
                    params![session_id, state.max_message_id, current_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((message_id, role, content, reasoning_content)) = row else {
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
                         WHERE session_id = ?1 AND id < ?2 AND id <= ?3
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
            messages,
            next_cursor,
            has_older,
        })
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

    fn load_messages(conn: &Connection, session_id: i64) -> Result<Vec<ConversationMessage>> {
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
                out.push(ConversationMessage::Chat(ChatMessage { role, content }));
            } else {
                if !ins.is_empty() {
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
                }
                if !outs.is_empty() {
                    out.push(ConversationMessage::ToolResults(outs));
                }
            }
        }

        Ok(out)
    }

    /// Append all ConversationMessages from one completed turn, decomposing
    /// AssistantToolCalls / ToolResults variants into the appropriate tables.
    /// Single transaction.
    pub fn append_turn(&self, session_uuid: &str, messages: &[ConversationMessage]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();

        // Resolve the integer session_id once. Fail loudly if the UUID is
        // unknown — we want an error here, not orphaned inserts.
        let session_id: i64 = conn
            .query_row(
                "SELECT id FROM acp_sessions WHERE session_uuid = ?1",
                params![session_uuid],
                |row| row.get(0),
            )
            .with_context(|| format!("unknown session_uuid: {session_uuid}"))?;

        let tx = conn
            .transaction()
            .context("Failed to begin append_turn transaction")?;

        // Track the most recent assistant message_id so a following
        // ToolResults variant can attach its 'out' rows back to it.
        let mut last_assistant_msg_id: Option<i64> = None;

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
                    if chat.role == "assistant" {
                        last_assistant_msg_id = Some(tx.last_insert_rowid());
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

        tx.execute(
            "UPDATE acp_sessions SET last_activity = ?1 WHERE id = ?2",
            params![now, session_id],
        )
        .context("Failed to update last_activity")?;

        tx.commit().context("Failed to commit append_turn")?;
        Ok(())
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

    /// Persist that an admin intentionally killed this ACP session. The
    /// transcript stays durable, but runtime rehydration must not revive it.
    pub fn mark_session_killed(&self, session_uuid: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE acp_sessions
                    SET killed_at = COALESCE(killed_at, ?1),
                        last_activity = ?1
                  WHERE session_uuid = ?2",
                params![now, session_uuid],
            )
            .context("Failed to mark ACP session killed")?;
        Ok(rows > 0)
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
                    role: self.role.clone(),
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
    use zeroclaw_api::model_provider::{ChatMessage, ToolCall, ToolResultMessage};

    fn open_store() -> (TempDir, AcpSessionStore) {
        let tmp = TempDir::new().unwrap();
        let store = AcpSessionStore::new(tmp.path()).unwrap();
        (tmp, store)
    }

    #[test]
    fn new_creates_all_four_tables() {
        let (_tmp, store) = open_store();
        let conn = store.conn.lock();
        for table in [
            "acp_sessions",
            "acp_messages",
            "acp_tool_calls",
            "acp_session_events",
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
    fn cursor_pages_newest_rows_and_walks_back_without_repeating() {
        let (_tmp, store) = open_store();
        store.create_session("cursor", "alpha", "/tmp").unwrap();
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
        store.create_session("chat-roles", "alpha", "/tmp").unwrap();
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
        store.create_session("tools", "alpha", "/tmp").unwrap();
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
        store.create_session("stable", "alpha", "/tmp").unwrap();
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
    fn cursor_rejects_tool_id_reuse_across_message_groups() {
        let (_tmp, store) = open_store();
        store.create_session("reuse", "alpha", "/tmp").unwrap();
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
        store.create_session("defer", "alpha", "/tmp").unwrap();
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
        store.create_session("bad-empty", "alpha", "/tmp").unwrap();
        store
            .append_turn(
                "bad-empty",
                &[ConversationMessage::AssistantToolCalls {
                    text: None,
                    tool_calls: Vec::new(),
                    reasoning_content: None,
                }],
            )
            .unwrap();
        let conn = store.conn.lock();
        let message_id: i64 = conn
            .query_row(
                "SELECT id FROM acp_messages WHERE session_id = (SELECT id FROM acp_sessions WHERE session_uuid = 'bad-empty')",
                [],
                |row| row.get(0),
            )
            .unwrap();
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
        store.create_session("one", "alpha", "/tmp").unwrap();
        store.create_session("two", "alpha", "/tmp").unwrap();
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
        store.create_session("large", "alpha", "/tmp").unwrap();
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
}
