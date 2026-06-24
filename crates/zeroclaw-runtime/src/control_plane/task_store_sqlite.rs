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
use super::task_registry::{TaskKind, TaskRecord, TaskRegistry, TaskStatus};

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
             PRAGMA temp_store = MEMORY;",
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
             CREATE INDEX IF NOT EXISTS idx_tasks_agent  ON tasks(agent);",
        )
        .context("create control-plane schema")?;
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
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({ "error": format!("{e}") })),
                "control-plane: skipping unreadable task row"
            ),
        }
    }
    out
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
