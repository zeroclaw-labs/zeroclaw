//! The single SQLite-backed [`TaskRegistry`] — EPIC A's durable index.

use std::path::Path;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::authority::is_authoritative;
use super::task_registry::{
    TaskKind, TaskProgress, TaskRecord, TaskRegistry, TaskSnapshot, TaskStatus,
    TerminalSettlementIntent,
};

mod goal;

const CONTROL_PLANE_SCHEMA_VERSION: i64 = 13;

pub struct SqliteTaskStore {
    conn: Mutex<Connection>,
}

impl SqliteTaskStore {
    /// Open (creating if absent) the control-plane DB at `<data_dir>/control_plane.db`.
    /// Additive: a fresh install gets an empty DB and today's behavior is unchanged.
    pub fn new(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create data dir {}", data_dir.display()))?;
        let db_path = data_dir.join("control_plane.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open control-plane DB: {}", db_path.display()))?;
        Self::init(conn)
    }

    /// In-memory store for unit tests.
    pub fn new_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().context("open in-memory control-plane DB")?)
    }

    #[cfg(test)]
    pub(crate) fn insert_malformed_terminal_settlement_intent_for_test(
        &self,
        task_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO terminal_settlement_intents
                (task_id, owner_pid, owner_boot_id, desired_status, artifact_path,
                 artifact_ref, artifact_sha256, terminal_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                task_id,
                999_999_i64,
                "boot-OLD",
                "unknown-status",
                "/tmp/malformed-settlement.json",
                Option::<String>::None,
                "00".repeat(32),
                Option::<String>::None,
            ],
        )
        .context("insert malformed terminal settlement intent for test")?;
        Ok(())
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;
             PRAGMA foreign_keys = ON;",
        )
        .context("set control-plane PRAGMAs")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                 id              TEXT PRIMARY KEY,
                 kind            TEXT NOT NULL,
                 agent           TEXT NOT NULL,
                 status          TEXT NOT NULL,
                 owner_pid       INTEGER NOT NULL DEFAULT 0,
                 owner_boot_id   TEXT NOT NULL DEFAULT '',
                 heartbeat_at    TEXT,
                 depth           INTEGER NOT NULL DEFAULT 0,
                 parent_id       TEXT,
                 originator_route TEXT,
                 originator_chain TEXT,
                 delivered       INTEGER NOT NULL DEFAULT 0,
                 idem_key        TEXT,
                 principal_id    TEXT,
                 session_id      TEXT,
                 execution_epoch INTEGER NOT NULL DEFAULT 0,
                 started_at      TEXT NOT NULL,
                 finished_at     TEXT,
                 output          TEXT,
                 error           TEXT,
                 progress        TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_tasks_agent  ON tasks(agent);
             CREATE INDEX IF NOT EXISTS idx_tasks_agent_kind_started
                ON tasks(agent, kind, started_at DESC);",
        )
        .context("create control-plane base schema")?;
        migrate_schema(&conn).context("migrate control-plane schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Admin enumeration — count this agent's records (mirrors AcpSessionStore's
    /// `count_*_by_agent`; used by alias-delete cascades / observability).
    pub fn count_by_agent(&self, agent: &str) -> Result<u64> {
        let conn = self.conn.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE agent = ?1",
                params![agent],
                |r| r.get(0),
            )
            .context("count tasks by agent")?;
        Ok(n as u64)
    }

    /// Delete an agent's ordinary records while retaining session-Goal history.
    /// A nonterminal session Goal refuses the entire alias-delete cascade step.
    pub fn delete_by_agent(&self, agent: &str) -> Result<u64> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin tasks by agent deletion")?;
        let has_session_goal: bool = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM tasks
                      WHERE agent = ?1 AND kind = 'goal'
                        AND session_id IS NOT NULL AND length(trim(session_id)) > 0
                        AND status IN ('running', 'paused')
                 )",
                params![agent],
                |row| row.get(0),
            )
            .context("classify session-bound Goal agent deletion")?;
        if has_session_goal {
            anyhow::bail!(
                "session-bound Goal tasks require guarded GoalTaskRegistry disposal; \
                 generic agent deletion is unavailable"
            );
        }
        let n = tx
            .execute(
                "DELETE FROM tasks WHERE agent = ?1
                   AND NOT (kind = 'goal' AND session_id IS NOT NULL
                            AND length(trim(session_id)) > 0)",
                params![agent],
            )
            .context("delete tasks by agent")?;
        tx.commit().context("commit tasks by agent deletion")?;
        Ok(n as u64)
    }
}

fn migrate_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("read control-plane schema version")?;
    if version < CONTROL_PLANE_SCHEMA_VERSION {
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .context("begin control-plane schema migration")?;
        add_column_if_missing(
            &tx,
            "tasks",
            "session_id",
            "ALTER TABLE tasks ADD COLUMN session_id TEXT",
        )?;
        add_column_if_missing(
            &tx,
            "tasks",
            "execution_epoch",
            "ALTER TABLE tasks ADD COLUMN execution_epoch INTEGER NOT NULL DEFAULT 0",
        )?;
        add_column_if_missing(
            &tx,
            "tasks",
            "originator_chain",
            "ALTER TABLE tasks ADD COLUMN originator_chain TEXT",
        )?;
        add_column_if_missing(
            &tx,
            "tasks",
            "progress",
            "ALTER TABLE tasks ADD COLUMN progress TEXT",
        )?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS terminal_settlement_intents (
                     task_id          TEXT PRIMARY KEY
                                      REFERENCES tasks(id) ON DELETE CASCADE,
                     owner_pid        INTEGER NOT NULL,
                     owner_boot_id    TEXT NOT NULL,
                     desired_status   TEXT NOT NULL,
                     artifact_path    TEXT NOT NULL,
                     artifact_ref     TEXT,
                     artifact_sha256  TEXT NOT NULL,
                     terminal_error   TEXT
                 );
                 CREATE INDEX IF NOT EXISTS idx_terminal_settlement_intents_owner
                    ON terminal_settlement_intents(owner_pid, owner_boot_id);",
        )
        .context("ensure terminal settlement schema")?;
        // Version 8 existed both as upstream terminal-settlement schema and as
        // a provisional Goal schema. Converge older databases through Goal's
        // additive final schema; its session index replaces the superseded
        // context uniqueness index without recreating it on legacy rows.
        goal::converge_schema(&tx)?;
        tx.execute_batch(&format!(
            "PRAGMA user_version = {CONTROL_PLANE_SCHEMA_VERSION};"
        ))
        .context("mark final control-plane schema version")?;
        tx.commit()
            .context("commit control-plane schema migration")?;
    }
    if version > CONTROL_PLANE_SCHEMA_VERSION {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "db_version": version,
                    "known_version": CONTROL_PLANE_SCHEMA_VERSION,
                })
            ),
            "control-plane DB was created by a newer schema version"
        );
    }
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("inspect {table} columns"))?;
    let mut rows = stmt
        .query([])
        .with_context(|| format!("query {table} columns"))?;
    while let Some(row) = rows
        .next()
        .with_context(|| format!("iterate {table} columns"))?
    {
        let name: String = row
            .get(1)
            .with_context(|| format!("decode {table} column name"))?;
        if name == column {
            return Ok(());
        }
    }
    conn.execute_batch(alter_sql)
        .with_context(|| format!("add {table}.{column}"))?;
    Ok(())
}

// ── serde<->TEXT helpers (reuse the snake_case derive, no hand-kept string tables) ──

fn kind_to_db(k: TaskKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "delegate".into())
}

fn status_to_db(s: TaskStatus) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "running".into())
}

fn kind_from_db(s: &str) -> Result<TaskKind> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .with_context(|| format!("unknown task kind {s:?}"))
}

fn status_from_db(s: &str) -> Result<TaskStatus> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .with_context(|| format!("unknown task status {s:?}"))
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
    let kind_s: String = row.get("kind")?;
    let status_s: String = row.get("status")?;
    // serde parse failures map to a SQLite conversion error; callers SKIP such rows
    // (collect_skipping_bad_rows) rather than failing the whole query. The column index
    // (`0`) is a placeholder — rusqlite has no by-name conversion-error ctor and the
    // index is not surfaced to the skip path (review nit #4).
    let kind = kind_from_db(&kind_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    let status = status_from_db(&status_s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    // A chain that does not parse is dropped, not surfaced as a read error: the
    // row keeps whatever `originator_route` says, so a creator-stamped row loses
    // ancestor access and nothing else. A row with no creator route either is
    // indistinguishable from a pre-chain legacy row after this decode; a
    // non-TEXT value in the column is still a conversion error above.
    let originator_chain: Vec<String> = match row.get::<_, Option<String>>("originator_chain")? {
        Some(raw) if !raw.is_empty() => match serde_json::from_str(&raw) {
            Ok(chain) => chain,
            Err(_) => {
                let task_id: String = row.get("id")?;
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "task_id": task_id,
                        })),
                    "control-plane: task originator_chain is unreadable and was ignored; the creator route alone governs access"
                );
                Vec::new()
            }
        },
        _ => Vec::new(),
    };
    Ok(TaskRecord {
        id: row.get("id")?,
        kind,
        agent: row.get("agent")?,
        status,
        owner_pid: row.get::<_, i64>("owner_pid")? as u32,
        owner_boot_id: row.get("owner_boot_id")?,
        heartbeat_at: row.get("heartbeat_at")?,
        depth: row.get::<_, i64>("depth")? as u32,
        parent_id: row.get("parent_id")?,
        originator_route: row.get("originator_route")?,
        originator_chain,
        delivered: row.get::<_, i64>("delivered")? != 0,
        idem_key: row.get("idem_key")?,
        principal_id: row.get("principal_id")?,
        session_id: row.get("session_id")?,
        execution_epoch: row.get("execution_epoch")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
    })
}

fn row_to_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskSnapshot> {
    Ok(TaskSnapshot {
        task: row_to_record(row)?,
        output: row.get("output")?,
        error: row.get("error")?,
        progress: progress_from_row(row)?,
    })
}

/// Progress is advisory: a value that does not parse reads as absent rather
/// than failing the snapshot that carries the task's lifecycle and output.
fn progress_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<TaskProgress>> {
    let Some(raw) = row.get::<_, Option<String>>("progress")? else {
        return Ok(None);
    };
    match serde_json::from_str(&raw) {
        Ok(progress) => Ok(Some(progress)),
        Err(_) => {
            let task_id: String = row.get("id")?;
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "task_id": task_id,
                    })),
                "control-plane: task progress is unreadable and was ignored"
            );
            Ok(None)
        }
    }
}

fn row_to_settlement_intent(row: &rusqlite::Row<'_>) -> rusqlite::Result<TerminalSettlementIntent> {
    let status_s: String = row.get("desired_status")?;
    let desired_status = status_from_db(&status_s).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(TerminalSettlementIntent {
        task_id: row.get("task_id")?,
        owner_pid: row.get::<_, i64>("owner_pid")? as u32,
        owner_boot_id: row.get("owner_boot_id")?,
        desired_status,
        artifact_path: row.get("artifact_path")?,
        artifact_ref: row.get("artifact_ref")?,
        artifact_sha256: row.get("artifact_sha256")?,
        terminal_error: row.get("terminal_error")?,
    })
}

fn validate_settlement_intent(intent: &TerminalSettlementIntent) -> Result<()> {
    anyhow::ensure!(
        intent.desired_status.is_terminal(),
        "terminal settlement intent for {} must use a terminal status",
        intent.task_id
    );
    anyhow::ensure!(
        !intent.artifact_path.is_empty(),
        "terminal settlement intent for {} has no artifact path",
        intent.task_id
    );
    if intent.desired_status == TaskStatus::Completed {
        anyhow::ensure!(
            intent
                .artifact_ref
                .as_deref()
                .is_some_and(|artifact_ref| !artifact_ref.is_empty()),
            "completed settlement intent for {} has no artifact reference",
            intent.task_id
        );
    }
    anyhow::ensure!(
        hex::decode(&intent.artifact_sha256)
            .map(|digest| digest.len() == 32)
            .unwrap_or(false),
        "terminal settlement intent for {} has an invalid SHA-256 digest",
        intent.task_id
    );
    Ok(())
}

fn delete_settlement_intent_record(
    conn: &Connection,
    intent: &TerminalSettlementIntent,
) -> Result<usize> {
    conn.execute(
        "DELETE FROM terminal_settlement_intents
          WHERE task_id = ?1
            AND owner_pid = ?2
            AND owner_boot_id = ?3
            AND desired_status = ?4
            AND artifact_path = ?5
            AND artifact_ref IS ?6
            AND artifact_sha256 = ?7
            AND terminal_error IS ?8",
        params![
            &intent.task_id,
            intent.owner_pid as i64,
            &intent.owner_boot_id,
            status_to_db(intent.desired_status),
            &intent.artifact_path,
            &intent.artifact_ref,
            &intent.artifact_sha256,
            &intent.terminal_error,
        ],
    )
    .context("delete terminal settlement intent")
}

fn persist_settlement_intent_record(
    conn: &mut Connection,
    intent: &TerminalSettlementIntent,
) -> Result<bool> {
    validate_settlement_intent(intent)?;
    let tx = conn
        .transaction()
        .context("begin settlement intent transaction")?;
    let task_is_active: bool = tx
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM tasks
                 WHERE id = ?1
                   AND owner_pid = ?2
                   AND owner_boot_id = ?3
                   AND status NOT IN ('completed','failed','cancelled','lost','timed_out')
            )",
            params![
                &intent.task_id,
                intent.owner_pid as i64,
                &intent.owner_boot_id
            ],
            |row| row.get::<_, i64>(0),
        )
        .context("check terminal settlement owner")?
        != 0;
    if !task_is_active {
        tx.commit()
            .context("finish inactive settlement intent check")?;
        return Ok(false);
    }

    let inserted = tx
        .execute(
            "INSERT INTO terminal_settlement_intents
                (task_id, owner_pid, owner_boot_id, desired_status, artifact_path,
                 artifact_ref, artifact_sha256, terminal_error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(task_id) DO NOTHING",
            params![
                &intent.task_id,
                intent.owner_pid as i64,
                &intent.owner_boot_id,
                status_to_db(intent.desired_status),
                &intent.artifact_path,
                &intent.artifact_ref,
                &intent.artifact_sha256,
                &intent.terminal_error,
            ],
        )
        .context("persist terminal settlement intent")?;
    if inserted == 1 {
        tx.commit().context("commit terminal settlement intent")?;
        return Ok(true);
    }

    let existing = tx
        .query_row(
            "SELECT * FROM terminal_settlement_intents WHERE task_id = ?1",
            params![&intent.task_id],
            row_to_settlement_intent,
        )
        .optional()
        .context("read existing terminal settlement intent")?;
    if let Some(existing) = existing {
        anyhow::ensure!(
            existing == intent.clone(),
            "conflicting terminal settlement intent for task {}",
            intent.task_id
        );
        tx.commit()
            .context("commit existing terminal settlement intent")?;
        return Ok(true);
    }

    tx.commit()
        .context("finish missing settlement intent check")?;
    Ok(false)
}

fn promote_settlement_record(
    conn: &mut Connection,
    intent: &TerminalSettlementIntent,
    resolved_status: TaskStatus,
    output: Option<String>,
    error: Option<String>,
) -> Result<bool> {
    anyhow::ensure!(
        resolved_status.is_terminal(),
        "terminal settlement resolution requires a terminal status"
    );
    let tx = conn
        .transaction()
        .context("begin terminal settlement promotion")?;
    let finished_at = chrono::Utc::now().to_rfc3339();
    let changed = tx
        .execute(
            "UPDATE tasks
                SET status = ?1,
                    output = ?2,
                    error = ?3,
                    finished_at = ?4
              WHERE id = ?5
                AND owner_pid = ?6
                AND owner_boot_id = ?7
                AND status NOT IN ('completed','failed','cancelled','lost','timed_out')
                AND EXISTS (
                    SELECT 1
                      FROM terminal_settlement_intents
                     WHERE task_id = ?5
                       AND owner_pid = ?6
                       AND owner_boot_id = ?7
                       AND desired_status = ?8
                       AND artifact_path = ?9
                       AND artifact_ref IS ?10
                       AND artifact_sha256 = ?11
                       AND terminal_error IS ?12
                )",
            params![
                status_to_db(resolved_status),
                output,
                error,
                finished_at,
                &intent.task_id,
                intent.owner_pid as i64,
                &intent.owner_boot_id,
                status_to_db(intent.desired_status),
                &intent.artifact_path,
                &intent.artifact_ref,
                &intent.artifact_sha256,
                &intent.terminal_error,
            ],
        )
        .context("promote terminal settlement")?;
    let _ = delete_settlement_intent_record(&tx, intent)?;
    tx.commit()
        .context("commit terminal settlement promotion")?;
    Ok(changed == 1)
}

/// Collect query rows, skipping and logging conversion failures while
/// propagating operational SQLite step errors. One unrecognised/corrupt record
/// (for example a forward-incompatible `kind` or `status`) must not starve the
/// reaper, but a failed database operation must never become partial success.
fn collect_skipping_bad_rows(rows: &mut rusqlite::Rows<'_>) -> rusqlite::Result<Vec<TaskRecord>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        match row_to_record(row) {
            Ok(rec) => out.push(rec),
            Err(e) => log_unreadable_task_row(e),
        }
    }
    Ok(out)
}

fn log_unreadable_task_row(error: rusqlite::Error) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
        "control-plane: skipping unreadable task row"
    );
}

/// Collect settlement intents while skipping conversion failures and
/// propagating operational SQLite step errors. Recovery metadata must not keep
/// ordinary task reconciliation from running, but a failed query cannot become
/// partial success.
fn collect_skipping_bad_settlement_intents(
    rows: &mut rusqlite::Rows<'_>,
) -> rusqlite::Result<Vec<TerminalSettlementIntent>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        match row_to_settlement_intent(row) {
            Ok(intent) => out.push(intent),
            Err(error) => log_unreadable_terminal_settlement_intent(error),
        }
    }
    Ok(out)
}

fn log_unreadable_terminal_settlement_intent(error: rusqlite::Error) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
        "control-plane: skipping unreadable terminal settlement intent"
    );
}

fn insert_task_record(conn: &Connection, rec: TaskRecord) -> Result<()> {
    // ON CONFLICT DO NOTHING, NOT INSERT OR REPLACE: re-registering an existing id
    // must be a true no-op, never clobber an already-recorded output/error/terminal
    // status back to NULL/running (review finding— the documented idempotency).
    // The chain is stored as a JSON string array; an empty chain stays NULL so
    // pre-chain rows and non-delegate rows are indistinguishable at rest.
    let originator_chain_db = if rec.originator_chain.is_empty() {
        None
    } else {
        serde_json::to_string(&rec.originator_chain).ok()
    };
    conn.execute(
        "INSERT INTO tasks
            (id, kind, agent, status, owner_pid, owner_boot_id, heartbeat_at, depth,
             parent_id, originator_route, originator_chain, delivered, idem_key, principal_id,
             session_id, execution_epoch, started_at, finished_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
         ON CONFLICT(id) DO NOTHING",
        params![
            rec.id,
            kind_to_db(rec.kind),
            rec.agent,
            status_to_db(rec.status),
            rec.owner_pid as i64,
            rec.owner_boot_id,
            rec.heartbeat_at,
            rec.depth as i64,
            rec.parent_id,
            rec.originator_route,
            originator_chain_db,
            rec.delivered as i64,
            rec.idem_key,
            rec.principal_id,
            rec.session_id,
            rec.execution_epoch,
            rec.started_at,
            rec.finished_at,
        ],
    )
    .context("insert task record")?;
    Ok(())
}

/// Generic task lifecycle APIs predate Goal epoch fencing. They remain valid
/// for unrelated and legacy rows but must not mutate a session-bound Goal.
fn reject_session_bound_goal_mutation(
    conn: &Connection,
    task_id: &str,
    operation: &str,
) -> Result<()> {
    let session_bound: bool = conn
        .query_row(
            "SELECT kind = 'goal' AND session_id IS NOT NULL
               AND length(trim(session_id)) > 0
               FROM tasks WHERE id = ?1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()
        .context("classify generic task lifecycle mutation")?
        .unwrap_or(false);
    if session_bound {
        anyhow::bail!(
            "session-bound Goal tasks require guarded GoalTaskRegistry APIs; {operation} is unavailable"
        );
    }
    Ok(())
}

fn update_task_status_record(
    conn: &Connection,
    id: &str,
    status: TaskStatus,
    output: Option<String>,
    error: Option<String>,
) -> Result<usize> {
    let finished_at = status
        .is_terminal()
        .then(|| chrono::Utc::now().to_rfc3339());
    let changed = conn
        .execute(
            "UPDATE tasks
                SET status = ?1,
                    output = COALESCE(?2, output),
                    error  = COALESCE(?3, error),
                    finished_at = COALESCE(?4, finished_at)
              WHERE id = ?5
                AND status NOT IN ('completed','failed','cancelled','lost','timed_out')",
            params![status_to_db(status), output, error, finished_at, id],
        )
        .context("update task status")?;
    if changed > 0 && status.is_terminal() {
        conn.execute(
            "DELETE FROM terminal_settlement_intents WHERE task_id = ?1",
            params![id],
        )
        .context("delete stale terminal settlement intent")?;
    }
    Ok(changed)
}

fn transition_task_terminal_record(
    conn: &mut Connection,
    id: &str,
    status: TaskStatus,
    output: Option<String>,
    error: Option<String>,
) -> Result<usize> {
    anyhow::ensure!(
        status.is_terminal(),
        "terminal transition requires a terminal status"
    );
    let tx = conn.transaction().context("begin terminal transition")?;
    let finished_at = chrono::Utc::now().to_rfc3339();
    let changed = tx
        .execute(
            "UPDATE tasks
                SET status = ?1,
                    output = ?2,
                    error = ?3,
                    finished_at = ?4
              WHERE id = ?5
                AND status NOT IN ('completed','failed','cancelled','lost','timed_out')",
            params![status_to_db(status), output, error, finished_at, id],
        )
        .context("transition task terminal")?;
    if changed == 1 {
        tx.execute(
            "DELETE FROM terminal_settlement_intents WHERE task_id = ?1",
            params![id],
        )
        .context("delete stale terminal settlement intent")?;
    }
    tx.commit().context("commit terminal transition")?;
    Ok(changed)
}

fn transition_task_terminal_if_owner_record(
    conn: &mut Connection,
    id: &str,
    owner_pid: u32,
    owner_boot_id: &str,
    status: TaskStatus,
    output: Option<String>,
    error: Option<String>,
) -> Result<usize> {
    anyhow::ensure!(
        status.is_terminal(),
        "terminal transition requires a terminal status"
    );
    let tx = conn
        .transaction()
        .context("begin owner-checked terminal transition")?;
    let finished_at = chrono::Utc::now().to_rfc3339();
    let changed = tx
        .execute(
            "UPDATE tasks
                SET status = ?1,
                    output = ?2,
                    error = ?3,
                    finished_at = ?4
              WHERE id = ?5
                AND owner_pid = ?6
                AND owner_boot_id = ?7
                AND status NOT IN ('completed','failed','cancelled','lost','timed_out')",
            params![
                status_to_db(status),
                output,
                error,
                finished_at,
                id,
                owner_pid as i64,
                owner_boot_id,
            ],
        )
        .context("transition owner-checked task terminal")?;
    if changed == 1 {
        tx.execute(
            "DELETE FROM terminal_settlement_intents WHERE task_id = ?1",
            params![id],
        )
        .context("delete stale terminal settlement intent")?;
    }
    tx.commit()
        .context("commit owner-checked terminal transition")?;
    Ok(changed)
}

fn claim_task_owner_record(
    conn: &Connection,
    id: &str,
    owner_pid: u32,
    owner_boot_id: &str,
) -> Result<usize> {
    conn.execute(
        "UPDATE tasks
            SET owner_pid = ?1,
                owner_boot_id = ?2,
                heartbeat_at = NULL
          WHERE id = ?3
            AND status NOT IN ('completed','failed','cancelled','lost','timed_out')",
        params![owner_pid as i64, owner_boot_id, id],
    )
    .context("claim task owner")
}

#[async_trait::async_trait]
impl TaskRegistry for SqliteTaskStore {
    async fn create(&self, rec: TaskRecord) -> Result<()> {
        let conn = self.conn.lock();
        if rec.kind == TaskKind::Goal {
            anyhow::bail!("Goal tasks require the session-bound GoalTaskRegistry admission API");
        }
        insert_task_record(&conn, rec)?;
        Ok(())
    }

    async fn heartbeat(&self, id: &str, owner_boot_id: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, id, "heartbeat")?;
        // Only the heart-beating owner refreshes; prevents a stale boot from
        // resurrecting liveness it does not own.
        conn.execute(
            "UPDATE tasks SET heartbeat_at = ?1
             WHERE id = ?2 AND owner_boot_id = ?3",
            params![now, id, owner_boot_id],
        )
        .context("heartbeat task")?;
        Ok(())
    }

    async fn record_progress(
        &self,
        id: &str,
        owner_boot_id: &str,
        progress: &TaskProgress,
    ) -> Result<bool> {
        let encoded = serde_json::to_string(progress).context("encode task progress")?;
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, id, "progress record")?;
        let changed = conn
            .execute(
                "UPDATE tasks SET progress = ?1, heartbeat_at = ?2
                 WHERE id = ?3 AND owner_boot_id = ?4 AND status = ?5",
                params![
                    encoded,
                    now,
                    id,
                    owner_boot_id,
                    status_to_db(TaskStatus::Running)
                ],
            )
            .context("record task progress")?;
        Ok(changed == 1)
    }

    async fn update_status(
        &self,
        id: &str,
        status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, id, "status update")?;
        update_task_status_record(&conn, id, status, output, error)?;
        Ok(())
    }

    async fn transition_terminal(
        &self,
        id: &str,
        status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, id, "terminal transition")?;
        Ok(transition_task_terminal_record(&mut conn, id, status, output, error)? == 1)
    }

    async fn transition_terminal_if_owner(
        &self,
        id: &str,
        owner_pid: u32,
        owner_boot_id: &str,
        status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, id, "owner-checked terminal transition")?;
        Ok(transition_task_terminal_if_owner_record(
            &mut conn,
            id,
            owner_pid,
            owner_boot_id,
            status,
            output,
            error,
        )? == 1)
    }

    async fn persist_terminal_settlement_intent(
        &self,
        intent: TerminalSettlementIntent,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, &intent.task_id, "terminal settlement intent")?;
        persist_settlement_intent_record(&mut conn, &intent)
    }

    async fn list_terminal_settlement_intents(&self) -> Result<Vec<TerminalSettlementIntent>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT * FROM terminal_settlement_intents
                 ORDER BY task_id",
            )
            .context("prepare list terminal settlement intents")?;
        let mut rows = stmt
            .query([])
            .context("query terminal settlement intents")?;
        collect_skipping_bad_settlement_intents(&mut rows)
            .context("decode terminal settlement intent rows")
    }

    async fn promote_terminal_settlement(
        &self,
        intent: &TerminalSettlementIntent,
        resolved_status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
        reject_session_bound_goal_mutation(
            &conn,
            &intent.task_id,
            "terminal settlement promotion",
        )?;
        promote_settlement_record(&mut conn, intent, resolved_status, output, error)
    }

    async fn discard_terminal_settlement_intent(
        &self,
        intent: &TerminalSettlementIntent,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, &intent.task_id, "terminal settlement discard")?;
        Ok(delete_settlement_intent_record(&conn, intent)? == 1)
    }

    async fn claim_owner(&self, id: &str, owner_pid: u32, owner_boot_id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, id, "owner claim")?;
        let tx = conn.transaction().context("begin task owner claim")?;
        tx.execute(
            "DELETE FROM terminal_settlement_intents
              WHERE task_id = ?1
                AND (owner_pid != ?2 OR owner_boot_id != ?3)",
            params![id, owner_pid as i64, owner_boot_id],
        )
        .context("delete prior-owner terminal settlement intent")?;
        claim_task_owner_record(&tx, id, owner_pid, owner_boot_id)?;
        tx.commit().context("commit task owner claim")?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<TaskRecord>> {
        let conn = self.conn.lock();
        let rec = conn
            .query_row(
                "SELECT * FROM tasks WHERE id = ?1",
                params![id],
                row_to_record,
            )
            .optional()
            .context("get task")?;
        Ok(rec)
    }

    async fn get_snapshot(&self, id: &str) -> Result<Option<TaskSnapshot>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT * FROM tasks WHERE id = ?1",
            params![id],
            row_to_snapshot,
        )
        .optional()
        .context("get task snapshot")
    }

    async fn list_running(&self) -> Result<Vec<TaskRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT * FROM tasks WHERE status = 'running'")
            .context("prepare list_running")?;
        let mut rows = stmt.query([]).context("query list_running")?;
        collect_skipping_bad_rows(&mut rows).context("decode list_running rows")
    }

    async fn list_by_agent(&self, agent: &str) -> Result<Vec<TaskRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT * FROM tasks WHERE agent = ?1 ORDER BY started_at DESC")
            .context("prepare list_by_agent")?;
        let mut rows = stmt.query(params![agent]).context("query list_by_agent")?;
        collect_skipping_bad_rows(&mut rows).context("decode list_by_agent rows")
    }

    async fn reconcile_lost(&self, id: &str, _now_boot_id: &str) -> Result<bool> {
        let rec = {
            let conn = self.conn.lock();
            conn.query_row(
                "SELECT * FROM tasks WHERE id = ?1",
                params![id],
                row_to_record,
            )
            .optional()
            .context("reconcile: load task")?
        };
        let Some(rec) = rec else { return Ok(false) };
        if rec.kind == TaskKind::Goal {
            return Ok(false);
        }
        // Never reclaim a terminal record, and never one a live owner still holds.
        if rec.status.is_terminal() || !is_authoritative(&rec) {
            return Ok(false);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .context("reconcile: begin lost transition")?;
        let changed = tx
            .execute(
                "UPDATE tasks
                    SET status = 'lost',
                        error = COALESCE(error, 'task owner is no longer available'),
                        finished_at = ?1
                  WHERE id = ?2
                    AND status = 'running'
                    AND owner_pid = ?3
                    AND owner_boot_id = ?4",
                params![now, id, rec.owner_pid as i64, rec.owner_boot_id],
            )
            .context("reconcile: mark lost")?;
        if changed == 1 {
            tx.execute(
                "DELETE FROM terminal_settlement_intents WHERE task_id = ?1",
                params![id],
            )
            .context("reconcile: delete stale terminal settlement intent")?;
        }
        tx.commit().context("reconcile: commit lost transition")?;
        Ok(changed == 1)
    }

    async fn reconcile_timed_out(
        &self,
        id: &str,
        owner_pid: u32,
        owner_boot_id: &str,
        heartbeat_at: &str,
    ) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        reject_session_bound_goal_mutation(&conn, id, "timeout reconciliation")?;
        let tx = conn
            .transaction()
            .context("reconcile: begin timeout transition")?;
        let changed = tx
            .execute(
                "UPDATE tasks
                    SET status = 'timed_out',
                        error = COALESCE(error, 'heartbeat timeout'),
                        finished_at = ?1
                  WHERE id = ?2
                    AND status = 'running'
                    AND owner_pid = ?3
                    AND owner_boot_id = ?4
                    AND heartbeat_at = ?5",
                params![now, id, owner_pid as i64, owner_boot_id, heartbeat_at],
            )
            .context("reconcile: mark timed out")?;
        if changed == 1 {
            tx.execute(
                "DELETE FROM terminal_settlement_intents WHERE task_id = ?1",
                params![id],
            )
            .context("reconcile: delete stale terminal settlement intent")?;
        }
        tx.commit()
            .context("reconcile: commit timeout transition")?;
        Ok(changed == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copy the released schema independently of the candidate's migration.
    /// DDL comes from control_plane/task_store_sqlite.rs::{init,migrate_schema}
    /// and control_plane/task_store_sqlite/goal.rs::migrate_schema before the
    /// session-bound Goal schema. Version 9 predates the progress column.
    fn create_upstream_progress_fixture(conn: &Connection, version: i64) {
        conn.execute_batch(
            r#"CREATE TABLE IF NOT EXISTS tasks (
                 id              TEXT PRIMARY KEY,
                 kind            TEXT NOT NULL,
                 agent           TEXT NOT NULL,
                 status          TEXT NOT NULL,
                 owner_pid       INTEGER NOT NULL DEFAULT 0,
                 owner_boot_id   TEXT NOT NULL DEFAULT '',
                 heartbeat_at    TEXT,
                 depth           INTEGER NOT NULL DEFAULT 0,
                 parent_id       TEXT,
                 originator_route TEXT,
                 originator_chain TEXT,
                 delivered       INTEGER NOT NULL DEFAULT 0,
                 idem_key        TEXT,
                 principal_id    TEXT,
                 started_at      TEXT NOT NULL,
                 finished_at     TEXT,
                 output          TEXT,
                 error           TEXT,
                 progress        TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_tasks_agent  ON tasks(agent);
             CREATE INDEX IF NOT EXISTS idx_tasks_agent_kind_started
                ON tasks(agent, kind, started_at DESC);
CREATE TABLE IF NOT EXISTS goal_tasks (
                 task_id        TEXT PRIMARY KEY
                                REFERENCES tasks(id) ON DELETE CASCADE,
                 objective      TEXT NOT NULL
             );
ALTER TABLE goal_tasks ADD COLUMN effective_token_limit INTEGER;
ALTER TABLE goal_tasks ADD COLUMN effective_cost_limit_usd REAL;
ALTER TABLE goal_tasks ADD COLUMN pause_reason TEXT;
ALTER TABLE goal_tasks ADD COLUMN pause_description TEXT;
ALTER TABLE goal_tasks ADD COLUMN blockers_json TEXT NOT NULL DEFAULT '[]';
CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_active_goal_context
                ON tasks(
                    agent,
                    COALESCE(originator_route, ''),
                    COALESCE(principal_id, '')
                )
                WHERE kind = 'goal'
                  AND status NOT IN ('completed','failed','cancelled','lost','timed_out');
CREATE TABLE IF NOT EXISTS task_continuation_contexts (
                 task_id      TEXT PRIMARY KEY
                              REFERENCES tasks(id) ON DELETE CASCADE,
                 context_json TEXT NOT NULL
             );
CREATE TRIGGER IF NOT EXISTS trg_goal_tasks_require_goal_kind
                BEFORE INSERT ON goal_tasks
                FOR EACH ROW
                WHEN COALESCE((SELECT kind FROM tasks WHERE id = NEW.task_id), '') != 'goal'
                BEGIN
                    SELECT RAISE(ABORT, 'goal_tasks.task_id must reference TaskKind::Goal');
                END;
CREATE TRIGGER IF NOT EXISTS trg_goal_tasks_effective_limits_insert
                BEFORE INSERT ON goal_tasks
                FOR EACH ROW
                WHEN (NEW.effective_token_limit IS NOT NULL AND NEW.effective_token_limit < 0)
                  OR (NEW.effective_cost_limit_usd IS NOT NULL
                      AND NOT (NEW.effective_cost_limit_usd >= 0.0
                               AND NEW.effective_cost_limit_usd <= 1.7976931348623157e308))
                BEGIN
                    SELECT RAISE(ABORT, 'goal effective limits must be non-negative finite values');
                END;
             CREATE TRIGGER IF NOT EXISTS trg_goal_tasks_effective_limits_update
                BEFORE UPDATE OF effective_token_limit, effective_cost_limit_usd ON goal_tasks
                FOR EACH ROW
                WHEN (NEW.effective_token_limit IS NOT NULL AND NEW.effective_token_limit < 0)
                  OR (NEW.effective_cost_limit_usd IS NOT NULL
                      AND NOT (NEW.effective_cost_limit_usd >= 0.0
                               AND NEW.effective_cost_limit_usd <= 1.7976931348623157e308))
                BEGIN
                    SELECT RAISE(ABORT, 'goal effective limits must be non-negative finite values');
                END;
             CREATE TRIGGER IF NOT EXISTS trg_task_continuation_contexts_require_goal_insert
                BEFORE INSERT ON task_continuation_contexts
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1
                      FROM tasks
                      JOIN goal_tasks ON goal_tasks.task_id = tasks.id
                     WHERE tasks.id = NEW.task_id
                       AND tasks.kind = 'goal'
                )
                BEGIN
                    SELECT RAISE(ABORT, 'task_continuation_contexts.task_id must reference a goal task');
                END;
             CREATE TRIGGER IF NOT EXISTS trg_task_continuation_contexts_require_goal_update
                BEFORE UPDATE OF task_id ON task_continuation_contexts
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1
                      FROM tasks
                      JOIN goal_tasks ON goal_tasks.task_id = tasks.id
                     WHERE tasks.id = NEW.task_id
                       AND tasks.kind = 'goal'
                )
                BEGIN
                    SELECT RAISE(ABORT, 'task_continuation_contexts.task_id must reference a goal task');
                END;
CREATE TABLE IF NOT EXISTS terminal_settlement_intents (
                 task_id          TEXT PRIMARY KEY
                                  REFERENCES tasks(id) ON DELETE CASCADE,
                 owner_pid        INTEGER NOT NULL,
                 owner_boot_id    TEXT NOT NULL,
                 desired_status   TEXT NOT NULL,
                 artifact_path    TEXT NOT NULL,
                 artifact_ref     TEXT,
                 artifact_sha256  TEXT NOT NULL,
                 terminal_error   TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_terminal_settlement_intents_owner
                ON terminal_settlement_intents(owner_pid, owner_boot_id);"#,
        )
        .expect("create upstream control-plane fixture schema");
        match version {
            9 => conn
                .execute_batch("ALTER TABLE tasks DROP COLUMN progress; PRAGMA user_version = 9;")
                .expect("select upstream schema nine"),
            10 => conn
                .execute_batch("PRAGMA user_version = 10;")
                .expect("select upstream schema ten"),
            _ => panic!("unsupported upstream progress fixture version"),
        }
    }

    fn progress_fixture_rows(conn: &Connection, sql: &str) -> Vec<Vec<rusqlite::types::Value>> {
        let mut statement = conn
            .prepare(sql)
            .expect("prepare progress fixture snapshot");
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .expect("query progress fixture snapshot")
            .collect::<rusqlite::Result<_>>()
            .expect("read progress fixture snapshot")
    }

    /// Released version-nine and version-ten databases converge without
    /// changing ordinary data, terminal receipts, or completed legacy Goals.
    #[tokio::test]
    async fn upstream_progress_schemas_preserve_rows_during_goal_convergence() {
        for version in [9, 10] {
            let directory = tempfile::tempdir().expect("create progress migration fixture");
            let path = directory.path().join("control_plane.db");
            let conn = Connection::open(&path).expect("open upstream fixture");
            create_upstream_progress_fixture(&conn, version);
            conn.execute_batch(
                "INSERT INTO tasks
                    (id, kind, agent, status, owner_pid, owner_boot_id,
                     heartbeat_at, originator_chain, started_at, finished_at,
                     output, error)
                 VALUES
                    ('delegate-running', 'delegate', 'main', 'running', 1,
                     'fixture-boot', 'fixture-heartbeat', '[]', 'fixture-start',
                     NULL, 'running output', NULL),
                    ('delegate-completed', 'delegate', 'main', 'completed', 1,
                     'fixture-boot', 'fixture-heartbeat', '[]', 'fixture-start',
                     'fixture-finish', 'retained result', NULL),
                    ('goal-running', 'goal', 'main', 'running', 1,
                     'fixture-boot', 'fixture-heartbeat', '[]', 'fixture-start',
                     NULL, NULL, NULL),
                    ('goal-completed', 'goal', 'main', 'completed', 1,
                     'fixture-boot', 'fixture-heartbeat', '[]', 'fixture-start',
                     'fixture-finish', 'completed legacy result', NULL);
                 INSERT INTO goal_tasks (task_id, objective, effective_token_limit)
                 VALUES ('goal-running', 'legacy objective', 100),
                        ('goal-completed', 'completed legacy objective', 200);
                 INSERT INTO terminal_settlement_intents
                    (task_id, owner_pid, owner_boot_id, desired_status,
                     artifact_path, artifact_ref, artifact_sha256, terminal_error)
                 VALUES ('delegate-completed', 1, 'fixture-boot', 'completed',
                         '/fixture/result.json', 'fixture:receipt',
                         '0000000000000000000000000000000000000000000000000000000000000000',
                         NULL);",
            )
            .expect("insert upstream fixture rows");
            let progress = TaskProgress {
                iterations: 7,
                ..TaskProgress::default()
            };
            if version == 10 {
                conn.execute(
                    "UPDATE tasks SET progress = ?1 WHERE id = 'delegate-running'",
                    params![serde_json::to_string(&progress).expect("serialize fixture progress")],
                )
                .expect("store valid upstream progress");
                conn.execute(
                    "UPDATE tasks SET progress = 'not json' WHERE id = 'delegate-completed'",
                    [],
                )
                .expect("store malformed upstream progress");
            }
            let original_columns: Vec<String> = conn
                .prepare("SELECT name FROM pragma_table_info('tasks') ORDER BY cid")
                .expect("prepare original-column snapshot")
                .query_map([], |row| row.get(0))
                .expect("query original-column snapshot")
                .collect::<rusqlite::Result<_>>()
                .expect("read original-column snapshot");
            // Names come only from the fixed fixture DDL, never from user data.
            let retained_query = format!(
                "SELECT {} FROM tasks WHERE id IN
                    ('delegate-running', 'delegate-completed', 'goal-completed')
                 ORDER BY id",
                original_columns.join(", ")
            );
            let retained_rows = progress_fixture_rows(&conn, &retained_query);
            let receipts = progress_fixture_rows(
                &conn,
                "SELECT * FROM terminal_settlement_intents ORDER BY task_id",
            );
            drop(conn);

            let store = SqliteTaskStore::new(directory.path()).expect("converge upstream schema");
            let running = store
                .get_snapshot("delegate-running")
                .await
                .expect("read migrated running delegate")
                .expect("running delegate retained");
            assert_eq!(
                running.progress.as_ref(),
                (version == 10).then_some(&progress)
            );
            let completed = store
                .get_snapshot("delegate-completed")
                .await
                .expect("read migrated completed delegate")
                .expect("completed delegate retained");
            assert!(completed.progress.is_none());
            assert_eq!(completed.output.as_deref(), Some("retained result"));
            let legacy = store
                .get_snapshot("goal-running")
                .await
                .expect("read reconciled legacy Goal")
                .expect("legacy Goal retained");
            assert_eq!(legacy.task.status, TaskStatus::Failed);
            assert_eq!(legacy.error.as_deref(), Some("legacy_goal_missing_session"));
            let verify = Connection::open(&path).expect("verify migrated fixture independently");
            let schema: i64 = verify
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .expect("read converged schema version");
            assert_eq!(schema, CONTROL_PLANE_SCHEMA_VERSION);
            let has_progress: bool = verify
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'progress')",
                    [],
                    |row| row.get(0),
                )
                .expect("verify progress column");
            assert!(has_progress);
            assert_eq!(
                progress_fixture_rows(&verify, &retained_query),
                retained_rows
            );
            assert_eq!(
                progress_fixture_rows(
                    &verify,
                    "SELECT * FROM terminal_settlement_intents ORDER BY task_id",
                ),
                receipts
            );
            drop(verify);
            drop(store);

            let reopened =
                SqliteTaskStore::new(directory.path()).expect("reopen converged fixture");
            let verify = Connection::open(&path).expect("verify retained rows after reopen");
            assert_eq!(
                progress_fixture_rows(&verify, &retained_query),
                retained_rows
            );
            assert_eq!(
                progress_fixture_rows(
                    &verify,
                    "SELECT * FROM terminal_settlement_intents ORDER BY task_id",
                ),
                receipts
            );
            drop(verify);
            assert!(
                reopened
                    .record_progress("delegate-running", "fixture-boot", &progress)
                    .await
                    .expect("ordinary owner can still record progress")
            );
            assert_eq!(
                reopened
                    .get_snapshot("delegate-running")
                    .await
                    .expect("read newly recorded ordinary progress")
                    .expect("running delegate retained after progress")
                    .progress,
                Some(progress)
            );
        }
    }

    fn rec(id: &str, agent: &str, owner_pid: u32, boot: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            kind: TaskKind::Delegate,
            agent: agent.into(),
            status: TaskStatus::Running,
            owner_pid,
            owner_boot_id: boot.into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: None,
            execution_epoch: 0,
            started_at: "2026-06-18T00:00:00Z".into(),
            finished_at: None,
        }
    }

    #[tokio::test]
    async fn create_get_roundtrip() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("a", "main", 1, "boot-1")).await.unwrap();
        let got = s.get("a").await.unwrap().unwrap();
        assert_eq!(got.id, "a");
        assert_eq!(got.kind, TaskKind::Delegate);
        assert_eq!(got.status, TaskStatus::Running);
        assert!(s.get("missing").await.unwrap().is_none());
    }

    #[test]
    fn version_seven_store_migrates_terminal_settlement_outbox_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteTaskStore::new(dir.path()).unwrap();
        {
            let conn = store.conn.lock();
            conn.execute_batch(
                "DROP TABLE terminal_settlement_intents;
                 PRAGMA user_version = 7;",
            )
            .unwrap();
        }
        drop(store);

        let reopened = SqliteTaskStore::new(dir.path()).unwrap();
        let conn = reopened.conn.lock();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let outbox_exists: i64 = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                     WHERE type = 'table' AND name = 'terminal_settlement_intents'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, CONTROL_PLANE_SCHEMA_VERSION);
        assert_eq!(outbox_exists, 1);
    }

    #[tokio::test]
    async fn originator_chain_round_trips_and_migrates() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        // A chained row round-trips root-first, ending at the creating caller.
        let mut chained = rec("chain", "main", 1, "boot-1");
        chained.originator_route = Some("middle".into());
        chained.originator_chain = vec!["root".into(), "middle".into()];
        s.create(chained).await.unwrap();
        let got = s.get("chain").await.unwrap().unwrap();
        assert_eq!(got.originator_route.as_deref(), Some("middle"));
        assert_eq!(
            got.originator_chain,
            vec!["root".to_string(), "middle".to_string()]
        );

        // An empty chain stores NULL and reads back empty.
        s.create(rec("no-chain", "main", 1, "boot-1"))
            .await
            .unwrap();
        let got = s.get("no-chain").await.unwrap().unwrap();
        assert!(got.originator_chain.is_empty());
        {
            let conn = s.conn.lock();
            let stored: Option<String> = conn
                .query_row(
                    "SELECT originator_chain FROM tasks WHERE id = ?1",
                    params!["no-chain"],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                stored.is_none(),
                "an empty chain is stored as NULL, got {stored:?}"
            );
        }

        // A v8 store (tasks table without the column) migrates to v9 on reopen.
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteTaskStore::new(dir.path()).unwrap();
        {
            let conn = store.conn.lock();
            conn.execute_batch(
                "ALTER TABLE tasks DROP COLUMN originator_chain;
                 PRAGMA user_version = 8;",
            )
            .unwrap();
        }
        drop(store);

        let reopened = SqliteTaskStore::new(dir.path()).unwrap();
        {
            let conn = reopened.conn.lock();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, CONTROL_PLANE_SCHEMA_VERSION);
            let has_chain_column: i64 = conn
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM pragma_table_info('tasks')
                         WHERE name = 'originator_chain'
                    )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                has_chain_column, 1,
                "the v9 migration must add originator_chain to an existing tasks table"
            );
        }

        // A malformed chain value fails closed to an empty chain, not an error.
        reopened
            .create(rec("migrated", "main", 1, "boot-1"))
            .await
            .unwrap();
        {
            let conn = reopened.conn.lock();
            conn.execute(
                "UPDATE tasks SET originator_chain = 'not json' WHERE id = ?1",
                params!["migrated"],
            )
            .unwrap();
        }
        let got = reopened.get("migrated").await.unwrap().unwrap();
        assert!(
            got.originator_chain.is_empty(),
            "an unreadable chain fails closed to the creator route: {got:?}"
        );
    }

    #[tokio::test]
    async fn update_status_sets_terminal_and_finished_at() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("a", "main", 1, "boot-1")).await.unwrap();
        s.update_status("a", TaskStatus::Completed, Some("done".into()), None)
            .await
            .unwrap();
        let got = s.get("a").await.unwrap().unwrap();
        assert_eq!(got.status, TaskStatus::Completed);
        assert!(got.finished_at.is_some());
    }

    #[tokio::test]
    async fn terminal_transition_atomically_records_the_winning_outcome() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("atomic", "main", 1, "boot-1")).await.unwrap();

        assert!(
            s.transition_terminal(
                "atomic",
                TaskStatus::Completed,
                Some("delegate_results/atomic.json".into()),
                None,
            )
            .await
            .unwrap()
        );

        let snapshot = s.get_snapshot("atomic").await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Completed);
        assert_eq!(
            snapshot.output.as_deref(),
            Some("delegate_results/atomic.json")
        );
        assert!(snapshot.error.is_none());
        assert!(snapshot.task.finished_at.is_some());
    }

    #[tokio::test]
    async fn owner_checked_terminal_transition_requires_the_current_owner() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("owner-match", "main", 7, "boot-old"))
            .await
            .unwrap();

        assert!(
            s.transition_terminal_if_owner(
                "owner-match",
                7,
                "boot-old",
                TaskStatus::Completed,
                Some("done".into()),
                None,
            )
            .await
            .unwrap()
        );
        assert_eq!(
            s.get("owner-match").await.unwrap().unwrap().status,
            TaskStatus::Completed
        );

        s.create(rec("owner-transfer", "main", 7, "boot-old"))
            .await
            .unwrap();
        s.claim_owner("owner-transfer", 42, "boot-new")
            .await
            .unwrap();

        assert!(
            !s.transition_terminal_if_owner(
                "owner-transfer",
                7,
                "boot-old",
                TaskStatus::Failed,
                None,
                Some("stale owner".into()),
            )
            .await
            .unwrap()
        );
        let transferred = s.get("owner-transfer").await.unwrap().unwrap();
        assert_eq!(transferred.status, TaskStatus::Running);
        assert_eq!(
            (transferred.owner_pid, transferred.owner_boot_id.as_str()),
            (42, "boot-new")
        );
        assert!(
            s.transition_terminal_if_owner(
                "owner-transfer",
                42,
                "boot-new",
                TaskStatus::Completed,
                Some("resumed".into()),
                None,
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn competing_terminal_transition_cannot_overwrite_the_winner() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("race", "main", 1, "boot-1")).await.unwrap();

        assert!(
            s.transition_terminal(
                "race",
                TaskStatus::Cancelled,
                None,
                Some("cancelled by user request".into()),
            )
            .await
            .unwrap()
        );
        assert!(
            !s.transition_terminal(
                "race",
                TaskStatus::Completed,
                Some("delegate_results/race.json".into()),
                None,
            )
            .await
            .unwrap()
        );

        let snapshot = s.get_snapshot("race").await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Cancelled);
        assert!(snapshot.output.is_none());
        assert_eq!(snapshot.error.as_deref(), Some("cancelled by user request"));
    }

    #[tokio::test]
    async fn timeout_reconciliation_requires_the_observed_owner_and_heartbeat() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("timeout-race", "main", 7, "boot-1");
        task.heartbeat_at = Some("2026-06-18T00:00:00Z".into());
        s.create(task).await.unwrap();

        assert!(
            !s.reconcile_timed_out("timeout-race", 7, "boot-1", "2026-06-18T00:00:01Z",)
                .await
                .unwrap()
        );
        assert_eq!(
            s.get("timeout-race").await.unwrap().unwrap().status,
            TaskStatus::Running
        );
        assert!(
            s.reconcile_timed_out("timeout-race", 7, "boot-1", "2026-06-18T00:00:00Z",)
                .await
                .unwrap()
        );
        assert_eq!(
            s.get("timeout-race").await.unwrap().unwrap().status,
            TaskStatus::TimedOut
        );
    }

    #[tokio::test]
    async fn list_running_and_by_agent() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("a", "main", 1, "b")).await.unwrap();
        s.create(rec("b", "main", 1, "b")).await.unwrap();
        s.create(rec("c", "other", 1, "b")).await.unwrap();
        s.update_status("b", TaskStatus::Completed, None, None)
            .await
            .unwrap();
        assert_eq!(s.list_running().await.unwrap().len(), 2); // a + c
        assert_eq!(s.list_by_agent("main").await.unwrap().len(), 2); // a + b
        assert_eq!(s.count_by_agent("main").unwrap(), 2);
    }

    #[tokio::test]
    async fn reconcile_lost_only_when_authoritative() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        // prior-boot orphan ⇒ reclaimable
        s.create(rec("orphan", "main", 999_999, "boot-OLD"))
            .await
            .unwrap();
        assert!(s.reconcile_lost("orphan", "boot-NEW").await.unwrap());
        assert_eq!(
            s.get("orphan").await.unwrap().unwrap().status,
            TaskStatus::Lost
        );

        // live same-boot owner ⇒ NOT reclaimable (split-brain guard)
        let me = std::process::id();
        s.create(rec("live", "main", me, "boot-NEW")).await.unwrap();
        assert!(!s.reconcile_lost("live", "boot-NEW").await.unwrap());
        assert_eq!(
            s.get("live").await.unwrap().unwrap().status,
            TaskStatus::Running
        );

        // already-terminal ⇒ no-op
        s.create(rec("done", "main", 0, "boot-OLD")).await.unwrap();
        s.update_status("done", TaskStatus::Completed, None, None)
            .await
            .unwrap();
        assert!(!s.reconcile_lost("done", "boot-NEW").await.unwrap());
    }

    #[tokio::test]
    async fn heartbeat_only_from_owner_boot() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("a", "main", 1, "boot-1")).await.unwrap();
        s.heartbeat("a", "boot-OTHER").await.unwrap(); // wrong boot: no-op
        assert!(s.get("a").await.unwrap().unwrap().heartbeat_at.is_none());
        s.heartbeat("a", "boot-1").await.unwrap(); // owner: stamps
        assert!(s.get("a").await.unwrap().unwrap().heartbeat_at.is_some());
    }

    #[tokio::test]
    async fn record_progress_is_owner_gated_and_running_only() {
        use super::super::task_registry::{TaskProgress, TaskProgressTool};
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("a", "main", 1, "boot-1")).await.unwrap();
        let progress = TaskProgress {
            last_activity_at: Some("2026-06-18T00:00:05Z".into()),
            iterations: 2,
            tools_completed: 1,
            last_tool: Some(TaskProgressTool {
                name: "shell".into(),
                started_at: Some("2026-06-18T00:00:04Z".into()),
                finished_at: Some("2026-06-18T00:00:05Z".into()),
                success: Some(true),
            }),
            timeout_budget_secs: Some(300),
            recent_tools: vec![TaskProgressTool {
                name: "shell".into(),
                ..TaskProgressTool::default()
            }],
            receipt_tail: vec!["zc-receipt:x".into()],
        };

        // Wrong boot: nothing written, heartbeat untouched.
        assert!(
            !s.record_progress("a", "boot-OTHER", &progress)
                .await
                .unwrap()
        );
        let snap = s.get_snapshot("a").await.unwrap().unwrap();
        assert!(snap.progress.is_none());
        assert!(snap.task.heartbeat_at.is_none());

        // Owner: round-trips and stamps the heartbeat.
        assert!(s.record_progress("a", "boot-1", &progress).await.unwrap());
        let snap = s.get_snapshot("a").await.unwrap().unwrap();
        assert_eq!(snap.progress.as_ref(), Some(&progress));
        assert!(snap.task.heartbeat_at.is_some());

        // Terminal row: no write, the last running-state progress stays.
        s.update_status("a", TaskStatus::Completed, Some("done".into()), None)
            .await
            .unwrap();
        let mut later = progress.clone();
        later.iterations = 99;
        assert!(!s.record_progress("a", "boot-1", &later).await.unwrap());
        let snap = s.get_snapshot("a").await.unwrap().unwrap();
        assert_eq!(snap.progress.as_ref().map(|p| p.iterations), Some(2));
        assert_eq!(snap.output.as_deref(), Some("done"));

        // A malformed value reads as absent, never as a snapshot error.
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE tasks SET progress = 'not json' WHERE id = ?1",
                params!["a"],
            )
            .unwrap();
        }
        let snap = s.get_snapshot("a").await.unwrap().unwrap();
        assert!(snap.progress.is_none());
        assert_eq!(snap.output.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn progress_column_migrates_from_v9() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteTaskStore::new(dir.path()).unwrap();
        {
            let conn = store.conn.lock();
            conn.execute_batch(
                "ALTER TABLE tasks DROP COLUMN progress;
                 PRAGMA user_version = 9;",
            )
            .unwrap();
        }
        drop(store);

        let reopened = SqliteTaskStore::new(dir.path()).unwrap();
        {
            let conn = reopened.conn.lock();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, CONTROL_PLANE_SCHEMA_VERSION);
            let has_column: i64 = conn
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM pragma_table_info('tasks')
                         WHERE name = 'progress'
                    )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                has_column, 1,
                "the v10 migration must add progress to an existing tasks table"
            );
        }
        reopened
            .create(rec("migrated", "main", 1, "boot-1"))
            .await
            .unwrap();
        let snap = reopened.get_snapshot("migrated").await.unwrap().unwrap();
        assert!(snap.progress.is_none());
    }

    #[tokio::test]
    async fn claim_owner_updates_canonical_owner_fields_for_resumed_task() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("a", "main", 1, "boot-old")).await.unwrap();

        s.claim_owner("a", 42, "boot-new").await.unwrap();

        let got = s.get("a").await.unwrap().unwrap();
        assert_eq!(got.owner_pid, 42);
        assert_eq!(got.owner_boot_id, "boot-new");
        assert!(got.heartbeat_at.is_none());
    }

    #[tokio::test]
    async fn claim_owner_removes_only_the_prior_owners_settlement_intent() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("a", "main", 1, "boot-old")).await.unwrap();
        let intent = TerminalSettlementIntent {
            task_id: "a".into(),
            owner_pid: 1,
            owner_boot_id: "boot-old".into(),
            desired_status: TaskStatus::Completed,
            artifact_path: "/tmp/a.json".into(),
            artifact_ref: Some("artifact:a.json".into()),
            artifact_sha256: "00".repeat(32),
            terminal_error: None,
        };
        assert!(s.persist_terminal_settlement_intent(intent).await.unwrap());

        s.claim_owner("a", 1, "boot-old").await.unwrap();
        assert_eq!(s.list_terminal_settlement_intents().await.unwrap().len(), 1);

        s.claim_owner("a", 42, "boot-new").await.unwrap();

        assert!(
            s.list_terminal_settlement_intents()
                .await
                .unwrap()
                .is_empty()
        );
        let got = s.get("a").await.unwrap().unwrap();
        assert_eq!(
            (got.owner_pid, got.owner_boot_id.as_str()),
            (42, "boot-new")
        );
    }
}
