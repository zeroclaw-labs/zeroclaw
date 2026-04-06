use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::fmt;
use std::path::Path;
use std::str::FromStr;

// ── Enums ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl WorkflowStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for WorkflowStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorkflowStatus {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => anyhow::bail!("unknown workflow status: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowType {
    Code,
    Browser,
}

impl WorkflowType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Browser => "browser",
        }
    }
}

impl fmt::Display for WorkflowType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorkflowType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "code" => Ok(Self::Code),
            "browser" => Ok(Self::Browser),
            other => anyhow::bail!("unknown workflow type: {other}"),
        }
    }
}

// ── Workflow struct ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Workflow {
    pub id: i64,
    pub name: String,
    pub workflow_type: WorkflowType,
    pub status: WorkflowStatus,
    pub stage_cur: i64,
    pub stage_total: i64,
    pub log_tail: Option<String>,
    pub result_url: Option<String>,
    pub error_msg: Option<String>,
    pub last_stage_changed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Store ──────────────────────────────────────────────────────────

pub struct WorkflowStore {
    conn: Mutex<Connection>,
}

impl WorkflowStore {
    /// Open (or create) the workflow database at `db_dir/workflows.db`.
    pub fn new(db_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(db_dir).context("create workflow db dir")?;
        let db_path = db_dir.join("workflows.db");

        let conn =
            Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA temp_store   = MEMORY;
             PRAGMA mmap_size    = 4194304;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflows (
                id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                name                  TEXT    NOT NULL,
                workflow_type         TEXT    NOT NULL,
                status                TEXT    NOT NULL DEFAULT 'pending',
                stage_cur             INTEGER NOT NULL DEFAULT 0,
                stage_total           INTEGER NOT NULL,
                log_tail              TEXT,
                result_url            TEXT,
                error_msg             TEXT,
                last_stage_changed_at TEXT,
                created_at            TEXT    NOT NULL,
                updated_at            TEXT    NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_workflows_status ON workflows(status);",
        )
        .context("initialize workflow schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create from an already-open in-memory connection (for tests).
    #[cfg(test)]
    fn new_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflows (
                id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                name                  TEXT    NOT NULL,
                workflow_type         TEXT    NOT NULL,
                status                TEXT    NOT NULL DEFAULT 'pending',
                stage_cur             INTEGER NOT NULL DEFAULT 0,
                stage_total           INTEGER NOT NULL,
                log_tail              TEXT,
                result_url            TEXT,
                error_msg             TEXT,
                last_stage_changed_at TEXT,
                created_at            TEXT    NOT NULL,
                updated_at            TEXT    NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_workflows_status ON workflows(status);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ── CRUD ───────────────────────────────────────────────────────

    pub fn insert(&self, name: &str, workflow_type: WorkflowType, stage_total: i64) -> Result<i64> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO workflows (name, workflow_type, stage_total, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, workflow_type.as_str(), stage_total, now, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get(&self, id: i64) -> Result<Option<Workflow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, workflow_type, status, stage_cur, stage_total,
                    log_tail, result_url, error_msg, last_stage_changed_at,
                    created_at, updated_at
             FROM workflows WHERE id = ?1",
        )?;
        let result = stmt.query_row(params![id], |row| Ok(row_to_workflow(row)));
        match result {
            Ok(w) => Ok(Some(w?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_active(&self) -> Result<Vec<Workflow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, workflow_type, status, stage_cur, stage_total,
                    log_tail, result_url, error_msg, last_stage_changed_at,
                    created_at, updated_at
             FROM workflows WHERE status IN ('pending', 'running')
             ORDER BY created_at ASC",
        )?;
        collect_workflows(&mut stmt, [])
    }

    pub fn list_all(&self) -> Result<Vec<Workflow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, workflow_type, status, stage_cur, stage_total,
                    log_tail, result_url, error_msg, last_stage_changed_at,
                    created_at, updated_at
             FROM workflows
             ORDER BY (status = 'running') DESC, created_at DESC",
        )?;
        collect_workflows(&mut stmt, [])
    }

    /// Update progress for a running workflow. No-op if status != 'running'.
    /// Returns the number of rows affected (0 or 1).
    pub fn update_stage(
        &self,
        id: i64,
        stage_cur: i64,
        log_tail: Option<&str>,
        last_stage_changed_at: DateTime<Utc>,
    ) -> Result<usize> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let changed = last_stage_changed_at.to_rfc3339();
        let rows = conn.execute(
            "UPDATE workflows
             SET stage_cur = ?1, log_tail = ?2, last_stage_changed_at = ?3, updated_at = ?4
             WHERE id = ?5 AND status = 'running'",
            params![stage_cur, log_tail, changed, now, id],
        )?;
        Ok(rows)
    }

    /// Mark a running workflow as done. No-op if status != 'running'.
    pub fn mark_done(&self, id: i64, result_url: Option<&str>) -> Result<usize> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE workflows SET status = 'done', result_url = ?1, updated_at = ?2
             WHERE id = ?3 AND status = 'running'",
            params![result_url, now, id],
        )?;
        Ok(rows)
    }

    /// Mark a pending or running workflow as failed. No-op otherwise.
    pub fn mark_failed(&self, id: i64, error_msg: Option<&str>) -> Result<usize> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE workflows SET status = 'failed', error_msg = ?1, updated_at = ?2
             WHERE id = ?3 AND status IN ('pending', 'running')",
            params![error_msg, now, id],
        )?;
        Ok(rows)
    }

    /// Retry a workflow: create a new pending copy, return the new id.
    /// Returns None if the source workflow doesn't exist.
    pub fn retry(&self, id: i64) -> Result<Option<i64>> {
        let original = self.get(id)?;
        let Some(w) = original else {
            return Ok(None);
        };
        let new_id = self.insert(&w.name, w.workflow_type, w.stage_total)?;
        Ok(Some(new_id))
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn row_to_workflow(row: &rusqlite::Row<'_>) -> Result<Workflow> {
    let wtype_str: String = row.get(2)?;
    let status_str: String = row.get(3)?;
    let created_str: String = row.get(10)?;
    let updated_str: String = row.get(11)?;
    let stage_changed_str: Option<String> = row.get(9)?;

    Ok(Workflow {
        id: row.get(0)?,
        name: row.get(1)?,
        workflow_type: WorkflowType::from_str(&wtype_str)?,
        status: WorkflowStatus::from_str(&status_str)?,
        stage_cur: row.get(4)?,
        stage_total: row.get(5)?,
        log_tail: row.get(6)?,
        result_url: row.get(7)?,
        error_msg: row.get(8)?,
        last_stage_changed_at: stage_changed_str.as_deref().map(parse_dt),
        created_at: parse_dt(&created_str),
        updated_at: parse_dt(&updated_str),
    })
}

fn collect_workflows<P: rusqlite::Params>(
    stmt: &mut rusqlite::Statement<'_>,
    params: P,
) -> Result<Vec<Workflow>> {
    let rows = stmt.query_map(params, |row| Ok(row_to_workflow(row)))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r??);
    }
    Ok(out)
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> WorkflowStore {
        WorkflowStore::new_in_memory().unwrap()
    }

    #[test]
    fn insert_and_get() {
        let s = store();
        let id = s.insert("deploy-app", WorkflowType::Code, 5).unwrap();
        let w = s.get(id).unwrap().unwrap();
        assert_eq!(w.name, "deploy-app");
        assert_eq!(w.workflow_type, WorkflowType::Code);
        assert_eq!(w.status, WorkflowStatus::Pending);
        assert_eq!(w.stage_cur, 0);
        assert_eq!(w.stage_total, 5);
        assert!(w.log_tail.is_none());
        assert!(w.result_url.is_none());
        assert!(w.error_msg.is_none());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let s = store();
        assert!(s.get(999).unwrap().is_none());
    }

    #[test]
    fn list_active_filters_correctly() {
        let s = store();
        let id1 = s.insert("a", WorkflowType::Code, 3).unwrap();
        let id2 = s.insert("b", WorkflowType::Browser, 2).unwrap();
        let _id3 = s.insert("c", WorkflowType::Code, 1).unwrap();

        // Move id1 to running
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id1],
            )
            .unwrap();
        }
        // Move id2 to done
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'done' WHERE id = ?1",
                params![id2],
            )
            .unwrap();
        }

        let active = s.list_active().unwrap();
        // id1 (running) and id3 (pending) should be active; id2 (done) excluded
        assert_eq!(active.len(), 2);
        let ids: Vec<i64> = active.iter().map(|w| w.id).collect();
        assert!(ids.contains(&id1));
        assert!(!ids.contains(&id2));
    }

    #[test]
    fn list_all_orders_running_first() {
        let s = store();
        let id1 = s.insert("first", WorkflowType::Code, 1).unwrap();
        let id2 = s.insert("second", WorkflowType::Code, 1).unwrap();

        // Make id2 running
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id2],
            )
            .unwrap();
        }

        let all = s.list_all().unwrap();
        assert_eq!(all.len(), 2);
        // Running (id2) should come first
        assert_eq!(all[0].id, id2);
        assert_eq!(all[1].id, id1);
    }

    #[test]
    fn update_stage_only_affects_running() {
        let s = store();
        let id = s.insert("w", WorkflowType::Code, 5).unwrap();

        // Pending — update_stage should be no-op
        let rows = s.update_stage(id, 2, Some("log"), Utc::now()).unwrap();
        assert_eq!(rows, 0);
        let w = s.get(id).unwrap().unwrap();
        assert_eq!(w.stage_cur, 0); // unchanged

        // Move to running
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }

        // Now update_stage should work
        let rows = s
            .update_stage(id, 3, Some("step 3 done"), Utc::now())
            .unwrap();
        assert_eq!(rows, 1);
        let w = s.get(id).unwrap().unwrap();
        assert_eq!(w.stage_cur, 3);
        assert_eq!(w.log_tail.as_deref(), Some("step 3 done"));
        assert!(w.last_stage_changed_at.is_some());
    }

    #[test]
    fn mark_done_only_from_running() {
        let s = store();
        let id = s.insert("w", WorkflowType::Browser, 3).unwrap();

        // Pending → mark_done should fail (no-op)
        let rows = s.mark_done(id, Some("http://result")).unwrap();
        assert_eq!(rows, 0);

        // Move to running, then mark_done
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let rows = s.mark_done(id, Some("http://result")).unwrap();
        assert_eq!(rows, 1);
        let w = s.get(id).unwrap().unwrap();
        assert_eq!(w.status, WorkflowStatus::Done);
        assert_eq!(w.result_url.as_deref(), Some("http://result"));
    }

    #[test]
    fn mark_done_noop_on_already_done() {
        let s = store();
        let id = s.insert("w", WorkflowType::Code, 1).unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        s.mark_done(id, None).unwrap();
        // Already done — second call is no-op
        let rows = s.mark_done(id, Some("url")).unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn mark_failed_from_pending_and_running() {
        let s = store();
        let id1 = s.insert("p", WorkflowType::Code, 1).unwrap();
        let id2 = s.insert("r", WorkflowType::Code, 1).unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id2],
            )
            .unwrap();
        }

        // Both should succeed
        assert_eq!(s.mark_failed(id1, Some("err1")).unwrap(), 1);
        assert_eq!(s.mark_failed(id2, Some("err2")).unwrap(), 1);

        assert_eq!(s.get(id1).unwrap().unwrap().status, WorkflowStatus::Failed);
        assert_eq!(s.get(id2).unwrap().unwrap().status, WorkflowStatus::Failed);
    }

    #[test]
    fn mark_failed_noop_on_done() {
        let s = store();
        let id = s.insert("w", WorkflowType::Code, 1).unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'done' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let rows = s.mark_failed(id, Some("too late")).unwrap();
        assert_eq!(rows, 0);
        assert_eq!(s.get(id).unwrap().unwrap().status, WorkflowStatus::Done);
    }

    #[test]
    fn retry_creates_new_pending_workflow() {
        let s = store();
        let id = s.insert("deploy", WorkflowType::Browser, 4).unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'failed' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }

        let new_id = s.retry(id).unwrap().unwrap();
        assert_ne!(new_id, id);

        let new_w = s.get(new_id).unwrap().unwrap();
        assert_eq!(new_w.name, "deploy");
        assert_eq!(new_w.workflow_type, WorkflowType::Browser);
        assert_eq!(new_w.stage_total, 4);
        assert_eq!(new_w.status, WorkflowStatus::Pending);
        assert_eq!(new_w.stage_cur, 0);
    }

    #[test]
    fn retry_nonexistent_returns_none() {
        let s = store();
        assert!(s.retry(999).unwrap().is_none());
    }

    #[test]
    fn mark_done_noop_on_cancelled_workflow() {
        // "cancelled" is simulated by mark_failed on a running workflow
        let s = store();
        let id = s.insert("w", WorkflowType::Code, 3).unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        // Cancel (fail) it
        s.mark_failed(id, Some("cancelled by user")).unwrap();
        // Now mark_done should be no-op
        let rows = s.mark_done(id, Some("http://result")).unwrap();
        assert_eq!(rows, 0);
        assert_eq!(s.get(id).unwrap().unwrap().status, WorkflowStatus::Failed);
    }

    #[test]
    fn update_stage_idempotent() {
        let s = store();
        let id = s.insert("w", WorkflowType::Code, 5).unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE workflows SET status = 'running' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        let ts = Utc::now();
        s.update_stage(id, 2, Some("log-a"), ts).unwrap();
        s.update_stage(id, 2, Some("log-a"), ts).unwrap();
        let w = s.get(id).unwrap().unwrap();
        assert_eq!(w.stage_cur, 2);
    }

    #[test]
    fn workflow_type_roundtrip() {
        assert_eq!(WorkflowType::from_str("code").unwrap(), WorkflowType::Code);
        assert_eq!(
            WorkflowType::from_str("browser").unwrap(),
            WorkflowType::Browser
        );
        assert!(WorkflowType::from_str("unknown").is_err());
    }

    #[test]
    fn workflow_status_roundtrip() {
        for status in [
            WorkflowStatus::Pending,
            WorkflowStatus::Running,
            WorkflowStatus::Done,
            WorkflowStatus::Failed,
        ] {
            assert_eq!(WorkflowStatus::from_str(status.as_str()).unwrap(), status);
        }
        assert!(WorkflowStatus::from_str("bogus").is_err());
    }
}
