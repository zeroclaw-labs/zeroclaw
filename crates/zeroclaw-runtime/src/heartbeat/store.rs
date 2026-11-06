//! SQLite persistence for heartbeat task execution history.
//! Mirrors the `cron/store.rs` pattern: fresh connection per call, schema
//! auto-created, output truncated, history pruned to a configurable limit.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
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
    if limit == 0 {
        return Ok(Vec::new());
    }

    with_connection(workspace_dir, |conn| {
        let lim = i64::try_from(limit).context("Run history limit overflow")?;
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

/// The durable watchdog row owns the monitoring baseline and tick generation.
/// Live HeartbeatMetrics are only observations and cannot re-arm an incident.
/// Initial startup is a baseline, not evidence that a tick completed; reopening
/// this store after reload/restart must not postpone an existing absence.
pub(crate) fn start_deadman(data_dir: &Path, now: DateTime<Utc>) -> Result<()> {
    with_connection(data_dir, |conn| {
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute(
            "INSERT OR IGNORE INTO heartbeat_deadman (id, observed_at, tick_sequence)
             VALUES (1, ?1, 0)",
            [now.timestamp()],
        )?;
        Ok(())
    })
}

/// Persist actual worker completion (successful, failed, empty, or skipped).
/// Returns whether this tick recovered an incident with an attempted alert.
pub(crate) fn record_completed_tick(data_dir: &Path, now: DateTime<Utc>) -> Result<bool> {
    with_connection(data_dir, |conn| {
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.query_row(
            "UPDATE heartbeat_deadman
             SET observed_at = ?1, tick_sequence = tick_sequence + 1
             WHERE id = 1
             RETURNING COALESCE(attempted_sequence = tick_sequence - 1, 0)",
            [now.timestamp()],
            |row| row.get(0),
        )
        .context("Failed to persist completed heartbeat tick")
    })
}

/// Atomically commit an unknown delivery attempt BEFORE contacting a channel.
/// Only one process/worker can claim this tick generation. Errors, timeouts,
/// cancellation, and process death all retain the claim: none authorize retry.
pub(crate) fn claim_deadman_alert(
    data_dir: &Path,
    now: DateTime<Utc>,
    timeout_minutes: u32,
) -> Result<Option<i64>> {
    if timeout_minutes == 0 {
        return Ok(None);
    }
    with_connection(data_dir, |conn| {
        conn.pragma_update(None, "synchronous", "FULL")?;
        let cutoff = now
            .timestamp()
            .saturating_sub(i64::from(timeout_minutes) * 60);
        conn.query_row(
            "UPDATE heartbeat_deadman
             SET attempted_sequence = tick_sequence, attempted_at = ?1, delivery_outcome = 'unknown'
             WHERE id = 1 AND observed_at < ?2
               AND (attempted_sequence IS NULL OR attempted_sequence != tick_sequence)
             RETURNING tick_sequence",
            params![now.timestamp(), cutoff],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to claim heartbeat deadman alert")
    })
}

pub(crate) fn finish_deadman_alert(data_dir: &Path, sequence: i64, delivered: bool) -> Result<()> {
    with_connection(data_dir, |conn| {
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute(
            "UPDATE heartbeat_deadman SET delivery_outcome = ?1
             WHERE id = 1 AND attempted_sequence = ?2",
            params![if delivered { "delivered" } else { "unknown" }, sequence],
        )?;
        Ok(())
    })
}

fn db_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("heartbeat").join("history.db")
}

fn with_connection<T>(workspace_dir: &Path, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let path = db_path(workspace_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create heartbeat directory: {}",
                parent.display().to_string()
            )
        })?;
    }

    let conn = Connection::open(&path).with_context(|| {
        format!(
            "Failed to open heartbeat history DB: {}",
            path.display().to_string()
        )
    })?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

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
         CREATE INDEX IF NOT EXISTS idx_hb_runs_task ON heartbeat_runs(task_text);
         CREATE TABLE IF NOT EXISTS heartbeat_deadman (
            id                 INTEGER PRIMARY KEY CHECK (id = 1),
            observed_at        INTEGER NOT NULL,
            tick_sequence      INTEGER NOT NULL,
            attempted_sequence INTEGER,
            attempted_at       INTEGER,
            delivery_outcome   TEXT CHECK (delivery_outcome IN ('unknown', 'delivered'))
         );",
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
    fn deadman_startup_threshold_repeats_restart_and_recovery() {
        let tmp = TempDir::new().unwrap();
        let base = Utc::now();
        start_deadman(tmp.path(), base).unwrap();
        assert_eq!(
            claim_deadman_alert(tmp.path(), base + ChronoDuration::minutes(45), 45).unwrap(),
            None
        );
        let overdue = base + ChronoDuration::minutes(46);
        let first = claim_deadman_alert(tmp.path(), overdue, 45)
            .unwrap()
            .unwrap();
        for minute in 47..120 {
            // All operations use fresh connections, so no in-memory guard can
            // make this test pass on behalf of the durable claim.
            start_deadman(tmp.path(), base + ChronoDuration::minutes(minute)).unwrap();
            assert_eq!(
                claim_deadman_alert(tmp.path(), base + ChronoDuration::minutes(minute), 45)
                    .unwrap(),
                None
            );
        }
        finish_deadman_alert(tmp.path(), first, true).unwrap();
        assert_eq!(
            claim_deadman_alert(tmp.path(), base + ChronoDuration::hours(3), 45).unwrap(),
            None
        );

        let recovered = base + ChronoDuration::hours(3);
        assert!(record_completed_tick(tmp.path(), recovered).unwrap());
        assert!(
            !record_completed_tick(tmp.path(), recovered + ChronoDuration::minutes(1)).unwrap()
        );
        let later = recovered + ChronoDuration::minutes(47);
        let second = claim_deadman_alert(tmp.path(), later, 45).unwrap().unwrap();
        assert_ne!(first, second);
        finish_deadman_alert(tmp.path(), first, true).unwrap();
        with_connection(tmp.path(), |conn| {
            let outcome: String =
                conn.query_row("SELECT delivery_outcome FROM heartbeat_deadman", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(
                outcome, "unknown",
                "late receipts cannot overwrite a later incident"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn deadman_mute_does_not_claim_or_rearm_and_clock_rollback_is_safe() {
        let tmp = TempDir::new().unwrap();
        let base = Utc::now();
        start_deadman(tmp.path(), base).unwrap();
        assert_eq!(
            claim_deadman_alert(tmp.path(), base - ChronoDuration::hours(1), 45).unwrap(),
            None
        );
        let overdue = base + ChronoDuration::hours(1);
        assert_eq!(claim_deadman_alert(tmp.path(), overdue, 0).unwrap(), None);
        let claimed = claim_deadman_alert(tmp.path(), overdue, 45)
            .unwrap()
            .unwrap();
        finish_deadman_alert(tmp.path(), claimed, false).unwrap();
        assert_eq!(claim_deadman_alert(tmp.path(), overdue, 0).unwrap(), None);
        assert_eq!(claim_deadman_alert(tmp.path(), overdue, 45).unwrap(), None);
        assert!(record_completed_tick(tmp.path(), overdue).unwrap());
        assert_eq!(
            claim_deadman_alert(tmp.path(), overdue + ChronoDuration::hours(1), 0).unwrap(),
            None
        );
        assert!(
            claim_deadman_alert(tmp.path(), overdue + ChronoDuration::hours(1), 45)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn deadman_concurrent_connections_claim_once() {
        let tmp = TempDir::new().unwrap();
        let base = Utc::now();
        start_deadman(tmp.path(), base).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = tmp.path().to_owned();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    claim_deadman_alert(&root, base + ChronoDuration::hours(1), 45)
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            1
        );
    }

    #[test]
    fn deadman_claim_child() {
        let Some(root) = std::env::var_os("ZEROCLAW_TEST_DEADMAN_CRASH_DIR") else {
            return;
        };
        let base = DateTime::from_timestamp(2_000_000_000, 0).unwrap();
        let root = PathBuf::from(root);
        start_deadman(&root, base).unwrap();
        assert!(
            claim_deadman_alert(&root, base + ChronoDuration::hours(1), 45)
                .unwrap()
                .is_some()
        );
        // No destructors or delivery receipt: model death after the durable
        // pre-send claim, including a delivery whose outcome was never saved.
        std::process::exit(23);
    }

    #[test]
    fn deadman_process_death_does_not_authorize_another_attempt() {
        let tmp = TempDir::new().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "heartbeat::store::tests::deadman_claim_child"])
            .env("ZEROCLAW_TEST_DEADMAN_CRASH_DIR", tmp.path())
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(23));
        let later = DateTime::from_timestamp(2_000_010_000, 0).unwrap();
        start_deadman(tmp.path(), later).unwrap();
        assert_eq!(claim_deadman_alert(tmp.path(), later, 45).unwrap(), None);
        assert!(record_completed_tick(tmp.path(), later).unwrap());
        assert!(
            claim_deadman_alert(tmp.path(), later + ChronoDuration::hours(1), 45)
                .unwrap()
                .is_some()
        );
    }

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
    fn list_runs_zero_limit_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let now = Utc::now();

        record_run(
            tmp.path(),
            "Task",
            "medium",
            now,
            now,
            "ok",
            Some("done"),
            10,
            0,
        )
        .unwrap();

        assert!(list_runs(tmp.path(), 0).unwrap().is_empty());
        assert_eq!(list_runs(tmp.path(), 10).unwrap().len(), 1);
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
