//! The single SQLite-backed [`TaskRegistry`] — EPIC A's durable index.
//!
//! Modelled directly on `zeroclaw_infra::acp_session_store::AcpSessionStore`
//! (`parking_lot::Mutex<Connection>` + WAL pragmas + `CREATE TABLE IF NOT EXISTS`),
//! so the supervision plane reuses the proven durability pattern rather than
//! inventing a new one. One `tasks` table indexes every supervised unit of work;
//! the producers' flat-JSON payloads stay where they are — this is the *index*.
//!
//! All methods are sync SQLite calls behind a `parking_lot::Mutex`; no `.await` is
//! held across the lock, so the `#[async_trait]` futures stay `Send`.

use std::path::Path;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use super::authority::is_authoritative;
use super::task_registry::{
    GoalBlocker, GoalPauseReason, GoalPauseState, GoalTaskRecord, TaskKind, TaskRecord,
    TaskRegistry, TaskStatus,
};

const CONTROL_PLANE_SCHEMA_VERSION: i64 = 3;

/// The durable task registry. `tasks.db` lives beside the other workspace DBs.
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
    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS goal_tasks (
                 task_id        TEXT PRIMARY KEY
                                REFERENCES tasks(id) ON DELETE CASCADE,
                 objective      TEXT NOT NULL
             );
             PRAGMA user_version = 1;",
        )
        .context("apply control-plane schema v1")?;
    }
    if version < 2 {
        add_column_if_missing(
            conn,
            "goal_tasks",
            "effective_token_limit",
            "ALTER TABLE goal_tasks ADD COLUMN effective_token_limit INTEGER",
        )?;
        add_column_if_missing(
            conn,
            "goal_tasks",
            "effective_cost_limit_usd",
            "ALTER TABLE goal_tasks ADD COLUMN effective_cost_limit_usd REAL",
        )?;
        conn.execute_batch("PRAGMA user_version = 2;")
            .context("mark control-plane schema v2")?;
    }
    if version < 3 {
        add_column_if_missing(
            conn,
            "goal_tasks",
            "pause_reason",
            "ALTER TABLE goal_tasks ADD COLUMN pause_reason TEXT",
        )?;
        add_column_if_missing(
            conn,
            "goal_tasks",
            "pause_description",
            "ALTER TABLE goal_tasks ADD COLUMN pause_description TEXT",
        )?;
        add_column_if_missing(
            conn,
            "goal_tasks",
            "blockers_json",
            "ALTER TABLE goal_tasks ADD COLUMN blockers_json TEXT NOT NULL DEFAULT '[]'",
        )?;
        conn.execute_batch("PRAGMA user_version = 3;")
            .context("mark control-plane schema v3")?;
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

fn pause_reason_to_db(reason: GoalPauseReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "needs_user_input".into())
}

fn pause_reason_from_db(value: Option<String>) -> Result<Option<GoalPauseReason>> {
    value
        .map(|value| {
            serde_json::from_value(serde_json::Value::String(value.clone()))
                .with_context(|| format!("unknown goal pause reason {value:?}"))
        })
        .transpose()
}

fn blockers_to_db(blockers: &[GoalBlocker]) -> Result<String> {
    serde_json::to_string(blockers).context("serialize goal blockers")
}

fn blockers_from_db(value: String) -> rusqlite::Result<Vec<GoalBlocker>> {
    serde_json::from_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })
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

fn row_to_goal_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<GoalTaskRecord> {
    let pause_reason = pause_reason_from_db(row.get("pause_reason")?).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(GoalTaskRecord {
        task_id: row.get("task_id")?,
        objective: row.get("objective")?,
        effective_token_limit: row
            .get::<_, Option<i64>>("effective_token_limit")?
            .map(|value| value as u64),
        effective_cost_limit_usd: row.get("effective_cost_limit_usd")?,
        pause_reason,
        pause_description: row.get("pause_description")?,
        blockers: blockers_from_db(row.get("blockers_json")?)?,
    })
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

#[async_trait::async_trait]
impl TaskRegistry for SqliteTaskStore {
    async fn create(&self, rec: TaskRecord) -> Result<()> {
        let conn = self.conn.lock();
        // ON CONFLICT DO NOTHING, NOT INSERT OR REPLACE: re-registering an existing id
        // must be a true no-op, never clobber an already-recorded output/error/terminal
        // status back to NULL/running (review finding #11 — the documented idempotency).
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
        let finished_at = status
            .is_terminal()
            .then(|| chrono::Utc::now().to_rfc3339());
        let conn = self.conn.lock();
        // Terminal-state guard: once a task has reached ANY terminal state this is a
        // no-op. Closes the reaper-sweep TOCTOU where a task that legitimately Completed
        // between the reaper's snapshot and its update could be clobbered back to
        // TimedOut (and its real finished_at lost). Mirrors reconcile_lost's
        // `AND status='running'` discipline (review finding #2).
        conn.execute(
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

    async fn latest_active_goal_for_agent(&self, agent: &str) -> Result<Option<TaskRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT * FROM tasks
              WHERE agent = ?1
                AND kind = 'goal'
                AND status NOT IN ('completed','failed','cancelled','lost','timed_out')
              ORDER BY started_at DESC",
            )
            .context("prepare latest active goal by agent")?;
        let rows = stmt
            .query_map(params![agent], row_to_record)
            .context("query latest active goal by agent")?;
        for row in rows {
            match row {
                Ok(task) => return Ok(Some(task)),
                Err(error) => log_unreadable_task_row(error),
            }
        }
        Ok(None)
    }

    async fn create_goal_task(&self, rec: GoalTaskRecord) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO goal_tasks
                (task_id, objective, effective_token_limit, effective_cost_limit_usd,
                 pause_reason, pause_description, blockers_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(task_id) DO NOTHING",
            params![
                rec.task_id,
                rec.objective,
                rec.effective_token_limit.map(|value| value as i64),
                rec.effective_cost_limit_usd,
                rec.pause_reason.map(pause_reason_to_db),
                rec.pause_description,
                blockers_to_db(&rec.blockers)?,
            ],
        )
        .context("insert goal task record")?;
        Ok(())
    }

    async fn get_goal_task(&self, task_id: &str) -> Result<Option<GoalTaskRecord>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT task_id, objective, effective_token_limit, effective_cost_limit_usd,
                    pause_reason, pause_description, blockers_json
             FROM goal_tasks WHERE task_id = ?1",
            params![task_id],
            row_to_goal_task,
        )
        .optional()
        .context("get goal task")
    }

    async fn update_goal_pause(&self, task_id: &str, pause: Option<GoalPauseState>) -> Result<()> {
        let conn = self.conn.lock();
        let (reason, description, blockers_json) = match pause {
            Some(pause) => (
                Some(pause_reason_to_db(pause.reason)),
                pause.description,
                blockers_to_db(&pause.blockers)?,
            ),
            None => (None, None, blockers_to_db(&[])?),
        };
        conn.execute(
            "UPDATE goal_tasks
                SET pause_reason = ?1,
                    pause_description = ?2,
                    blockers_json = ?3
              WHERE task_id = ?4",
            params![reason, description, blockers_json, task_id],
        )
        .context("update goal pause state")?;
        Ok(())
    }

    async fn reconcile_lost(&self, id: &str, now_boot_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let rec = conn
            .query_row(
                "SELECT * FROM tasks WHERE id = ?1",
                params![id],
                row_to_record,
            )
            .optional()
            .context("reconcile: load task")?;
        let Some(rec) = rec else { return Ok(false) };
        // Never reclaim a terminal record, and never one a live owner still holds.
        if rec.status.is_terminal() || !is_authoritative(&rec, now_boot_id) {
            return Ok(false);
        }
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE tasks SET status = 'lost', finished_at = ?1
              WHERE id = ?2 AND status = 'running'",
            params![now, id],
        )
        .context("reconcile: mark lost")?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::task_registry::GoalBlockerKind;

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
    async fn latest_active_goal_for_agent_resolves_in_sql() {
        let s = SqliteTaskStore::new_in_memory().unwrap();

        let mut old_goal = rec("old-goal", "main", 1, "boot-1");
        old_goal.kind = TaskKind::Goal;
        old_goal.started_at = "2026-06-18T00:00:00Z".into();
        s.create(old_goal).await.unwrap();

        let mut newer_terminal_goal = rec("done-goal", "main", 1, "boot-1");
        newer_terminal_goal.kind = TaskKind::Goal;
        newer_terminal_goal.started_at = "2026-06-20T00:00:00Z".into();
        s.create(newer_terminal_goal).await.unwrap();
        s.update_status("done-goal", TaskStatus::Completed, None, None)
            .await
            .unwrap();

        let mut newest_delegate = rec("newest-delegate", "main", 1, "boot-1");
        newest_delegate.started_at = "2026-06-21T00:00:00Z".into();
        s.create(newest_delegate).await.unwrap();

        let mut latest_active_goal = rec("latest-goal", "main", 1, "boot-1");
        latest_active_goal.kind = TaskKind::Goal;
        latest_active_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(latest_active_goal).await.unwrap();

        let got = s
            .latest_active_goal_for_agent("main")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "latest-goal");
        assert!(
            s.latest_active_goal_for_agent("other")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn latest_active_goal_for_agent_skips_unreadable_latest_candidate() {
        let s = SqliteTaskStore::new_in_memory().unwrap();

        let mut older_valid_goal = rec("older-valid-goal", "main", 1, "boot-1");
        older_valid_goal.kind = TaskKind::Goal;
        older_valid_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(older_valid_goal).await.unwrap();

        let mut unreadable_newer_goal = rec("unreadable-newer-goal", "main", 1, "boot-1");
        unreadable_newer_goal.kind = TaskKind::Goal;
        unreadable_newer_goal.started_at = "2026-06-20T00:00:00Z".into();
        s.create(unreadable_newer_goal).await.unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE tasks SET status = 'future_paused' WHERE id = ?1",
                params!["unreadable-newer-goal"],
            )
            .unwrap();
        }

        let got = s
            .latest_active_goal_for_agent("main")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "older-valid-goal");
    }

    #[tokio::test]
    async fn goal_task_extension_roundtrips_without_duplicating_lifecycle() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-1", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        task.status = TaskStatus::Paused;
        s.create(task).await.unwrap();
        s.create_goal_task(GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship goal mode".into(),
            effective_token_limit: Some(10_000),
            effective_cost_limit_usd: Some(1.25),
            pause_reason: Some(GoalPauseReason::NeedsUserInput),
            pause_description: Some("waiting for operator".into()),
            blockers: vec![GoalBlocker {
                kind: GoalBlockerKind::NeedsUserInput,
                message: "Need operator answer".into(),
                payload: Some(serde_json::json!({"question": "continue?"})),
            }],
        })
        .await
        .unwrap();

        let task = s.get("goal-1").await.unwrap().unwrap();
        let goal = s.get_goal_task("goal-1").await.unwrap().unwrap();

        assert_eq!(task.kind, TaskKind::Goal);
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.task_id, task.id);
        assert_eq!(goal.objective, "ship goal mode");
        assert_eq!(goal.effective_token_limit, Some(10_000));
        assert_eq!(goal.effective_cost_limit_usd, Some(1.25));
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
        assert_eq!(
            goal.pause_description.as_deref(),
            Some("waiting for operator")
        );
        assert_eq!(goal.blockers.len(), 1);

        s.update_goal_pause("goal-1", None).await.unwrap();
        let resumed = s.get_goal_task("goal-1").await.unwrap().unwrap();
        assert!(resumed.pause_reason.is_none());
        assert!(resumed.pause_description.is_none());
        assert!(resumed.blockers.is_empty());
    }

    #[tokio::test]
    async fn goal_task_requires_canonical_task_record() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let err = s
            .create_goal_task(GoalTaskRecord {
                task_id: "missing".into(),
                objective: "orphan objective".into(),
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: None,
                pause_description: None,
                blockers: Vec::new(),
            })
            .await
            .unwrap_err();

        assert!(format!("{err:#}").contains("FOREIGN KEY"));
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
}
