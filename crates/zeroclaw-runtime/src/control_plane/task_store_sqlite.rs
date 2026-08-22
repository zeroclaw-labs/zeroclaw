//! The single SQLite-backed [`TaskRegistry`] — EPIC A's durable index.

use std::path::Path;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use super::authority::is_authoritative;
use super::task_registry::{
    TaskKind, TaskRecord, TaskRegistry, TaskSnapshot, TaskStatus, TerminalSettlementIntent,
};

mod goal;

const CONTROL_PLANE_SCHEMA_VERSION: i64 = 8;

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
                 delivered       INTEGER NOT NULL DEFAULT 0,
                 idem_key        TEXT,
                 principal_id    TEXT,
                 started_at      TEXT NOT NULL,
                 finished_at     TEXT,
                 output          TEXT,
                 error           TEXT
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

    /// Admin enumeration — delete this agent's records (alias-delete cascade).
    pub fn delete_by_agent(&self, agent: &str) -> Result<u64> {
        let conn = self.conn.lock();
        let n = conn
            .execute("DELETE FROM tasks WHERE agent = ?1", params![agent])
            .context("delete tasks by agent")?;
        Ok(n as u64)
    }
}

fn migrate_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("read control-plane schema version")?;
    goal::migrate_schema(conn, version)?;
    if version < 8 {
        conn.execute_batch(
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
                ON terminal_settlement_intents(owner_pid, owner_boot_id);
             PRAGMA user_version = 8;",
        )
        .context("apply control-plane schema v8")?;
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
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("query {table} columns"))?;
    let exists = rows.any(|name| matches!(name, Ok(name) if name == column));
    if !exists {
        conn.execute_batch(alter_sql)
            .with_context(|| format!("add {table}.{column}"))?;
    }
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
        delivered: row.get::<_, i64>("delivered")? != 0,
        idem_key: row.get("idem_key")?,
        principal_id: row.get("principal_id")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
    })
}

fn row_to_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskSnapshot> {
    Ok(TaskSnapshot {
        task: row_to_record(row)?,
        output: row.get("output")?,
        error: row.get("error")?,
    })
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

/// Collect query rows, SKIPPING (and logging) any single row that fails to convert —
/// one unrecognised/corrupt record (e.g. a forward-incompat `kind`/`status` written by a
/// newer binary) must not fail the whole enumeration and starve the reaper (finding #3).
fn collect_skipping_bad_rows<I>(rows: I) -> Vec<TaskRecord>
where
    I: Iterator<Item = rusqlite::Result<TaskRecord>>,
{
    let mut out = Vec::new();
    for r in rows {
        match r {
            Ok(rec) => out.push(rec),
            Err(e) => log_unreadable_task_row(e),
        }
    }
    out
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

fn insert_task_record(conn: &Connection, rec: TaskRecord) -> Result<()> {
    // ON CONFLICT DO NOTHING, NOT INSERT OR REPLACE: re-registering an existing id
    // must be a true no-op, never clobber an already-recorded output/error/terminal
    // status back to NULL/running (review finding— the documented idempotency).
    conn.execute(
        "INSERT INTO tasks
            (id, kind, agent, status, owner_pid, owner_boot_id, heartbeat_at, depth,
             parent_id, originator_route, delivered, idem_key, principal_id,
             started_at, finished_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
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
            rec.delivered as i64,
            rec.idem_key,
            rec.principal_id,
            rec.started_at,
            rec.finished_at,
        ],
    )
    .context("insert task record")?;
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
        insert_task_record(&conn, rec)?;
        Ok(())
    }

    async fn heartbeat(&self, id: &str, owner_boot_id: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock();
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

    async fn update_status(
        &self,
        id: &str,
        status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<()> {
        let conn = self.conn.lock();
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
        Ok(transition_task_terminal_record(&mut conn, id, status, output, error)? == 1)
    }

    async fn persist_terminal_settlement_intent(
        &self,
        intent: TerminalSettlementIntent,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
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
        let rows = stmt
            .query_map([], row_to_settlement_intent)
            .context("query terminal settlement intents")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("read terminal settlement intents")
    }

    async fn promote_terminal_settlement(
        &self,
        intent: &TerminalSettlementIntent,
        resolved_status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
        promote_settlement_record(&mut conn, intent, resolved_status, output, error)
    }

    async fn discard_terminal_settlement_intent(
        &self,
        intent: &TerminalSettlementIntent,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        Ok(delete_settlement_intent_record(&conn, intent)? == 1)
    }

    async fn claim_owner(&self, id: &str, owner_pid: u32, owner_boot_id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
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
        let rows = stmt
            .query_map([], row_to_record)
            .context("query list_running")?;
        Ok(collect_skipping_bad_rows(rows))
    }

    async fn list_by_agent(&self, agent: &str) -> Result<Vec<TaskRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT * FROM tasks WHERE agent = ?1 ORDER BY started_at DESC")
            .context("prepare list_by_agent")?;
        let rows = stmt
            .query_map(params![agent], row_to_record)
            .context("query list_by_agent")?;
        Ok(collect_skipping_bad_rows(rows))
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
            delivered: false,
            idem_key: None,
            principal_id: None,
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
