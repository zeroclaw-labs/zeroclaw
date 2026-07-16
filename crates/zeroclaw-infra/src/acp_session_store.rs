//! ACP session persistence.
//!
//! Storage shape:
//!
//! ```text
//! acp_sessions
//!   ├── acp_messages (FK by integer id)
//!   │     └── acp_tool_calls (FK by integer id, two rows per call:
//!   │                          one event_kind='in', one 'out')
//!   └── acp_session_events
//! ```
//!
//! Token tracking: `acp_sessions.token_count` holds the most recently
//! provider-reported `input_tokens` (total prompt size). Replace-on-write
//! after every turn. The TUI ctx bar reads this on resume.
//!
//! Enums (Rust-side): `ToolEventKind` is internal to this module — callers
//! invoke `append_tool_call_in` / `append_tool_call_out` as distinct methods
//! and never see the enum. `Action` and `EventOutcome` from `zeroclaw_log`
//! are the canonical taxonomies for `acp_session_events`.

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

    /// Record a new session. Returns the integer `id` assigned by SQLite.
    pub fn create_session(
        &self,
        session_uuid: &str,
        agent_alias: &str,
        workspace_dir: &str,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO acp_sessions
               (session_uuid, agent_alias, workspace_dir, token_count, created_at, last_activity)
             VALUES (?1, ?2, ?3, 0, ?4, ?4)",
            params![session_uuid, agent_alias, workspace_dir, now],
        )
        .context("Failed to create ACP session")?;
        Ok(conn.last_insert_rowid())
    }

    /// Load session metadata and full message history for restore.
    /// Returns `None` if the session_uuid is not found.
    pub fn load_session(&self, session_uuid: &str) -> Result<Option<AcpSessionData>> {
        let conn = self.conn.lock();

        let row = conn.query_row(
            "SELECT id, agent_alias, workspace_dir, token_count, created_at, last_activity
             FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        );

        let (session_id, agent_alias, workspace_dir, token_count, created_at_s, last_activity_s) =
            match row {
                Ok(r) => r,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(e) => return Err(e).context("Failed to query ACP session"),
            };

        let created_at = parse_ts(&created_at_s, "created_at", session_uuid);
        let last_activity = parse_ts(&last_activity_s, "last_activity", session_uuid);

        // Reconstruct ConversationMessages by walking acp_messages in id order
        // and, for each assistant row, joining its tool calls from acp_tool_calls
        // (event_kind='in' for the call args).
        //
        // Tool results land as their own ConversationMessage::ToolResults at the
        // position of the LAST 'out' row in the result-batch. The replay strategy
        // is: every contiguous run of 'out' rows for a given message_id becomes
        // one ToolResults message inserted between the assistant's
        // AssistantToolCalls and the next message.
        let messages = Self::load_messages(&conn, session_id)?;

        Ok(Some(AcpSessionData {
            session_uuid: session_uuid.to_string(),
            agent_alias,
            workspace_dir,
            token_count: token_count.max(0) as u64,
            created_at,
            last_activity,
            messages,
        }))
    }

    /// Load only durable ACP rows that are allowed to become live sessions.
    /// Killed rows keep their transcript for history/export but are terminal
    /// for runtime restore paths.
    pub fn load_session_for_restore(&self, session_uuid: &str) -> Result<AcpSessionRestore> {
        let conn = self.conn.lock();

        let row = conn.query_row(
            "SELECT id, agent_alias, workspace_dir, token_count, created_at, last_activity, killed_at
             FROM acp_sessions WHERE session_uuid = ?1",
            params![session_uuid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        );

        let (
            session_id,
            agent_alias,
            workspace_dir,
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
                        // canonicalization (PR #7345).
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
                        // Look up tool_name from the matching 'in' row so the
                        // 'out' row carries it too. Outcome is 'unknown' at
                        // this layer — `ConversationMessage::ToolResults`
                        // doesn't tell us whether the tool succeeded or
                        // failed. Future wiring from the dispatcher will
                        // call a dedicated method to record outcome.
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

    /// Overwrite the session's `token_count`. Called after every turn with
    /// the latest provider-reported `input_tokens`.
    ///
    /// Returns an error if `session_uuid` does not exist — a silent zero-row
    /// UPDATE would mask a race where the session was deleted out from
    /// under us, which the caller almost certainly wants to log.
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

    // ── per-agent cascade (agent deletion, #7175) ───────────────────────────

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
    /// returning the row count. The agent-rename cascade (#7468) keeps the
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
        assert_eq!(data.token_count, 0);
        assert!(data.messages.is_empty());
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
