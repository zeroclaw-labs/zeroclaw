//! SQLite persistence for heartbeat task execution history.
//!
//! Mirrors the `cron/store.rs` pattern: fresh connection per call, schema
//! auto-created, output truncated, history pruned to a configurable limit.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const TRUNCATED_MARKER: &str = "\n...[truncated]";

/// A single heartbeat task execution record.
#[derive(Debug, Clone)]
pub struct HeartbeatRun {
    pub id: i64,
    pub task_text: String,
    pub task_priority: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: String, // "ok" or "error"
    pub output: Option<String>,
    pub duration_ms: i64,
}

/// Record a heartbeat task execution and prune old entries.
pub fn record_run(
    workspace_dir: &Path,
    task_text: &str,
    task_priority: &str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    status: &str,
    output: Option<&str>,
    duration_ms: i64,
    max_history: u32,
) -> Result<()> {
    let bounded_output = output.map(truncate_output);
    with_connection(workspace_dir, |conn| {
        let tx = conn.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO heartbeat_runs
                (task_text, task_priority, started_at, finished_at, status, output, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                task_text,
                task_priority,
                started_at.to_rfc3339(),
                finished_at.to_rfc3339(),
                status,
                bounded_output.as_deref(),
                duration_ms,
            ],
        )
        .context("Failed to insert heartbeat run")?;

        let keep = i64::from(max_history.max(1));
        tx.execute(
            "DELETE FROM heartbeat_runs
             WHERE id NOT IN (
                 SELECT id FROM heartbeat_runs
                 ORDER BY started_at DESC, id DESC
                 LIMIT ?1
             )",
            params![keep],
        )
        .context("Failed to prune heartbeat run history")?;

        tx.commit()
            .context("Failed to commit heartbeat run transaction")?;
        Ok(())
    })
}

/// List the most recent heartbeat runs.
pub fn list_runs(workspace_dir: &Path, limit: usize) -> Result<Vec<HeartbeatRun>> {
    with_connection(workspace_dir, |conn| {
        let lim = i64::try_from(limit.max(1)).context("Run history limit overflow")?;
        let mut stmt = conn.prepare(
            "SELECT id, task_text, task_priority, started_at, finished_at, status, output, duration_ms
             FROM heartbeat_runs
             ORDER BY started_at DESC, id DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![lim], |row| {
            Ok(HeartbeatRun {
                id: row.get(0)?,
                task_text: row.get(1)?,
                task_priority: row.get(2)?,
                started_at: parse_rfc3339(&row.get::<_, String>(3)?).map_err(sql_err)?,
                finished_at: parse_rfc3339(&row.get::<_, String>(4)?).map_err(sql_err)?,
                status: row.get(5)?,
                output: row.get(6)?,
                duration_ms: row.get(7)?,
            })
        })?;

        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    })
}

/// Get aggregate stats: (total_runs, total_ok, total_error).
pub fn run_stats(workspace_dir: &Path) -> Result<(u64, u64, u64)> {
    with_connection(workspace_dir, |conn| {
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM heartbeat_runs", [], |r| r.get(0))?;
        let ok: i64 = conn.query_row(
            "SELECT COUNT(*) FROM heartbeat_runs WHERE status = 'ok'",
            [],
            |r| r.get(0),
        )?;
        let err: i64 = conn.query_row(
            "SELECT COUNT(*) FROM heartbeat_runs WHERE status = 'error'",
            [],
            |r| r.get(0),
        )?;
        #[allow(clippy::cast_sign_loss)]
        Ok((total as u64, ok as u64, err as u64))
    })
}

fn db_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("heartbeat").join("history.db")
}

fn with_connection<T>(workspace_dir: &Path, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let path = db_path(workspace_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create heartbeat directory: {}", parent.display())
        })?;
    }

    let conn = Connection::open(&path)
        .with_context(|| format!("Failed to open heartbeat history DB: {}", path.display()))?;

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;

         CREATE TABLE IF NOT EXISTS heartbeat_runs (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            task_text      TEXT NOT NULL,
            task_priority  TEXT NOT NULL,
            started_at     TEXT NOT NULL,
            finished_at    TEXT NOT NULL,
            status         TEXT NOT NULL,
            output         TEXT,
            duration_ms    INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_hb_runs_started ON heartbeat_runs(started_at);
         CREATE INDEX IF NOT EXISTS idx_hb_runs_task ON heartbeat_runs(task_text);",
    )
    .context("Failed to initialize heartbeat history schema")?;

    f(&conn)
}

fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_string();
    }

    if MAX_OUTPUT_BYTES <= TRUNCATED_MARKER.len() {
        return TRUNCATED_MARKER.to_string();
    }

    let mut cutoff = MAX_OUTPUT_BYTES - TRUNCATED_MARKER.len();
    while cutoff > 0 && !output.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let mut truncated = output[..cutoff].to_string();
    truncated.push_str(TRUNCATED_MARKER);
    truncated
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("Invalid RFC3339 timestamp in heartbeat DB: {raw}"))?;
    Ok(parsed.with_timezone(&Utc))
}

fn sql_err(err: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(err.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    #[test]
    fn record_and_list_runs() {
        let tmp = TempDir::new().unwrap();
        let base = Utc::now();

        for i in 0..3 {
            let start = base + ChronoDuration::seconds(i);
            let end = start + ChronoDuration::milliseconds(100);
            record_run(
                tmp.path(),
                &format!("Task {i}"),
                "medium",
                start,
                end,
                "ok",
                Some("done"),
                100,
                50,
            )
            .unwrap();
        }

        let runs = list_runs(tmp.path(), 10).unwrap();
        assert_eq!(runs.len(), 3);
        // Most recent first
        assert!(runs[0].task_text.contains('2'));
    }

    #[test]
    fn prunes_old_runs() {
        let tmp = TempDir::new().unwrap();
        let base = Utc::now();

        for i in 0..5 {
            let start = base + ChronoDuration::seconds(i);
            let end = start + ChronoDuration::milliseconds(50);
            record_run(
                tmp.path(),
                "Task",
                "high",
                start,
                end,
                "ok",
                None,
                50,
                2, // keep only 2
            )
            .unwrap();
        }

        let runs = list_runs(tmp.path(), 10).unwrap();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn run_stats_counts_correctly() {
        let tmp = TempDir::new().unwrap();
        let now = Utc::now();

        record_run(tmp.path(), "A", "high", now, now, "ok", None, 10, 50).unwrap();
        record_run(
            tmp.path(),
            "B",
            "low",
            now,
            now,
            "error",
            Some("fail"),
            20,
            50,
        )
        .unwrap();
        record_run(tmp.path(), "C", "medium", now, now, "ok", None, 15, 50).unwrap();

        let (total, ok, err) = run_stats(tmp.path()).unwrap();
        assert_eq!(total, 3);
        assert_eq!(ok, 2);
        assert_eq!(err, 1);
    }

    #[test]
    fn truncates_large_output() {
        let tmp = TempDir::new().unwrap();
        let now = Utc::now();
        let big = "x".repeat(MAX_OUTPUT_BYTES + 512);

        record_run(
            tmp.path(),
            "T",
            "medium",
            now,
            now,
            "ok",
            Some(&big),
            10,
            50,
        )
        .unwrap();

        let runs = list_runs(tmp.path(), 1).unwrap();
        let stored = runs[0].output.as_deref().unwrap_or_default();
        assert!(stored.ends_with(TRUNCATED_MARKER));
        assert!(stored.len() <= MAX_OUTPUT_BYTES);
    }
}
