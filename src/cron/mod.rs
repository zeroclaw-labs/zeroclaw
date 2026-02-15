use crate::config::Config;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use rusqlite::{params, Connection};
use std::str::FromStr;
use uuid::Uuid;

pub mod scheduler;

#[derive(Debug, Clone)]
pub struct CronJob {
    pub id: String,
    pub expression: String,
    pub command: String,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub one_shot: bool,
}

#[allow(clippy::needless_pass_by_value)]
pub fn handle_command(command: crate::CronCommands, config: &Config) -> Result<()> {
    match command {
        crate::CronCommands::List => {
            let jobs = list_jobs(config)?;
            if jobs.is_empty() {
                println!("No scheduled tasks yet.");
                println!("\nUsage:");
                println!("  zeroclaw cron add '0 9 * * *' 'agent -m \"Good morning!\"'");
                return Ok(());
            }

            println!("🕒 Scheduled jobs ({}):", jobs.len());
            for job in jobs {
                let last_run = job
                    .last_run
                    .map_or_else(|| "never".into(), |d| d.to_rfc3339());
                let last_status = job.last_status.unwrap_or_else(|| "n/a".into());
                println!(
                    "- {} | {} | next={} | last={} ({})\n    cmd: {}",
                    job.id,
                    job.expression,
                    job.next_run.to_rfc3339(),
                    last_run,
                    last_status,
                    job.command
                );
            }
            Ok(())
        }
        crate::CronCommands::Add {
            expression,
            command,
        } => {
            let job = add_job(config, &expression, &command)?;
            println!("✅ Added cron job {}", job.id);
            println!("  Expr: {}", job.expression);
            println!("  Next: {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            Ok(())
        }
        crate::CronCommands::Remove { id } => {
            remove_job(config, &id)?;
            println!("✅ Removed cron job {id}");
            Ok(())
        }
    }
}

pub fn add_job(config: &Config, expression: &str, command: &str) -> Result<CronJob> {
    let now = Utc::now();
    let next_run = next_run_for(expression, now)?;
    let id = Uuid::new_v4().to_string();

    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO cron_jobs (id, expression, command, created_at, next_run, one_shot)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![
                id,
                expression,
                command,
                now.to_rfc3339(),
                next_run.to_rfc3339()
            ],
        )
        .context("Failed to insert cron job")?;
        Ok(())
    })?;

    Ok(CronJob {
        id,
        expression: expression.to_string(),
        command: command.to_string(),
        next_run,
        last_run: None,
        last_status: None,
        one_shot: false,
    })
}

pub fn add_one_shot_job(config: &Config, run_at: DateTime<Utc>, command: &str) -> Result<CronJob> {
    let now = Utc::now();
    let id = Uuid::new_v4().to_string();

    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO cron_jobs (id, expression, command, created_at, next_run, one_shot)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![id, "@once", command, now.to_rfc3339(), run_at.to_rfc3339()],
        )
        .context("Failed to insert one-shot job")?;
        Ok(())
    })?;

    Ok(CronJob {
        id,
        expression: "@once".to_string(),
        command: command.to_string(),
        next_run: run_at,
        last_run: None,
        last_status: None,
        one_shot: true,
    })
}

pub fn get_job(config: &Config, id: &str) -> Result<Option<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, next_run, last_run, last_status, one_shot
             FROM cron_jobs WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map(params![id], |row| Ok(parse_job_row(row)))?;

        match rows.next() {
            Some(Ok(job_result)) => Ok(Some(job_result?)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    })
}

pub fn list_jobs(config: &Config) -> Result<Vec<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, next_run, last_run, last_status, one_shot
             FROM cron_jobs ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map([], |row| Ok(parse_job_row(row)))?;

        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row??);
        }
        Ok(jobs)
    })
}

pub fn remove_job(config: &Config, id: &str) -> Result<()> {
    let changed = with_connection(config, |conn| {
        conn.execute("DELETE FROM cron_jobs WHERE id = ?1", params![id])
            .context("Failed to delete cron job")
    })?;

    if changed == 0 {
        anyhow::bail!("Cron job '{id}' not found");
    }

    Ok(())
}

pub fn due_jobs(config: &Config, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, next_run, last_run, last_status, one_shot
             FROM cron_jobs WHERE next_run <= ?1 ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map(params![now.to_rfc3339()], |row| Ok(parse_job_row(row)))?;

        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row??);
        }
        Ok(jobs)
    })
}

pub fn reschedule_after_run(
    config: &Config,
    job: &CronJob,
    success: bool,
    output: &str,
) -> Result<()> {
    let now = Utc::now();
    let next_run = next_run_for(&job.expression, now)?;
    let status = if success { "ok" } else { "error" };

    with_connection(config, |conn| {
        conn.execute(
            "UPDATE cron_jobs
             SET next_run = ?1, last_run = ?2, last_status = ?3, last_output = ?4
             WHERE id = ?5",
            params![
                next_run.to_rfc3339(),
                now.to_rfc3339(),
                status,
                output,
                job.id
            ],
        )
        .context("Failed to update cron job run state")?;
        Ok(())
    })
}

fn next_run_for(expression: &str, from: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let normalized = normalize_expression(expression)?;
    let schedule = Schedule::from_str(&normalized)
        .with_context(|| format!("Invalid cron expression: {expression}"))?;
    schedule
        .after(&from)
        .next()
        .ok_or_else(|| anyhow::anyhow!("No future occurrence for expression: {expression}"))
}

fn normalize_expression(expression: &str) -> Result<String> {
    let expression = expression.trim();
    let field_count = expression.split_whitespace().count();

    match field_count {
        // standard crontab syntax: minute hour day month weekday
        5 => Ok(format!("0 {expression}")),
        // crate-native syntax includes seconds (+ optional year)
        6 | 7 => Ok(expression.to_string()),
        _ => anyhow::bail!(
            "Invalid cron expression: {expression} (expected 5, 6, or 7 fields, got {field_count})"
        ),
    }
}

/// Parse a row from the cron_jobs table into a CronJob.
/// Expects columns: id, expression, command, next_run, last_run, last_status, one_shot
fn parse_job_row(row: &rusqlite::Row<'_>) -> Result<CronJob> {
    let id: String = row.get(0)?;
    let expression: String = row.get(1)?;
    let command: String = row.get(2)?;
    let next_run_raw: String = row.get(3)?;
    let last_run_raw: Option<String> = row.get(4)?;
    let last_status: Option<String> = row.get(5)?;
    let one_shot_int: i32 = row.get(6)?;

    Ok(CronJob {
        id,
        expression,
        command,
        next_run: parse_rfc3339(&next_run_raw)?,
        last_run: match last_run_raw {
            Some(raw) => Some(parse_rfc3339(&raw)?),
            None => None,
        },
        last_status,
        one_shot: one_shot_int != 0,
    })
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("Invalid RFC3339 timestamp in cron DB: {raw}"))?;
    Ok(parsed.with_timezone(&Utc))
}

fn with_connection<T>(config: &Config, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_path = config.workspace_dir.join("cron").join("jobs.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create cron directory: {}", parent.display()))?;
    }

    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open cron DB: {}", db_path.display()))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cron_jobs (
            id          TEXT PRIMARY KEY,
            expression  TEXT NOT NULL,
            command     TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            next_run    TEXT NOT NULL,
            last_run    TEXT,
            last_status TEXT,
            last_output TEXT,
            one_shot    INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_cron_jobs_next_run ON cron_jobs(next_run);",
    )
    .context("Failed to initialize cron schema")?;

    // Migration: add one_shot column to existing databases (silently ignore if already present)
    let _ =
        conn.execute_batch("ALTER TABLE cron_jobs ADD COLUMN one_shot INTEGER NOT NULL DEFAULT 0");

    f(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    #[test]
    fn add_job_accepts_five_field_expression() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "*/5 * * * *", "echo ok").unwrap();

        assert_eq!(job.expression, "*/5 * * * *");
        assert_eq!(job.command, "echo ok");
    }

    #[test]
    fn add_job_rejects_invalid_field_count() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let err = add_job(&config, "* * * *", "echo bad").unwrap_err();
        assert!(err.to_string().contains("expected 5, 6, or 7 fields"));
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "*/10 * * * *", "echo roundtrip").unwrap();
        let listed = list_jobs(&config).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, job.id);

        remove_job(&config, &job.id).unwrap();
        assert!(list_jobs(&config).unwrap().is_empty());
    }

    #[test]
    fn due_jobs_filters_by_timestamp() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let _job = add_job(&config, "* * * * *", "echo due").unwrap();

        let due_now = due_jobs(&config, Utc::now()).unwrap();
        assert!(due_now.is_empty(), "new job should not be due immediately");

        let far_future = Utc::now() + ChronoDuration::days(365);
        let due_future = due_jobs(&config, far_future).unwrap();
        assert_eq!(due_future.len(), 1, "job should be due in far future");
    }

    #[test]
    fn reschedule_after_run_persists_last_status_and_last_run() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "*/15 * * * *", "echo run").unwrap();
        reschedule_after_run(&config, &job, false, "failed output").unwrap();

        let listed = list_jobs(&config).unwrap();
        let stored = listed.iter().find(|j| j.id == job.id).unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert!(stored.last_run.is_some());
    }

    #[test]
    fn get_job_found() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "*/5 * * * *", "echo found").unwrap();
        let fetched = get_job(&config, &job.id).unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, job.id);
        assert_eq!(fetched.command, "echo found");
        assert!(!fetched.one_shot);
    }

    #[test]
    fn get_job_not_found() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let fetched = get_job(&config, "nonexistent-id").unwrap();
        assert!(fetched.is_none());
    }

    #[test]
    fn add_one_shot_job_sets_fields() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let run_at = Utc::now() + ChronoDuration::hours(1);
        let job = add_one_shot_job(&config, run_at, "echo once").unwrap();

        assert_eq!(job.expression, "@once");
        assert_eq!(job.command, "echo once");
        assert!(job.one_shot);
        assert!((job.next_run - run_at).num_seconds().abs() < 2);

        // Verify persisted correctly
        let fetched = get_job(&config, &job.id).unwrap().unwrap();
        assert!(fetched.one_shot);
        assert_eq!(fetched.expression, "@once");
    }

    #[test]
    fn one_shot_column_migration() {
        // Simulate a pre-migration database by creating the table without one_shot,
        // then calling with_connection which runs the migration.
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let db_path = config.workspace_dir.join("cron").join("jobs.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

        // Create old-schema table manually
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE cron_jobs (
                    id TEXT PRIMARY KEY,
                    expression TEXT NOT NULL,
                    command TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    next_run TEXT NOT NULL,
                    last_run TEXT,
                    last_status TEXT,
                    last_output TEXT
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO cron_jobs (id, expression, command, created_at, next_run)
                 VALUES ('old-job', '* * * * *', 'echo old', '2025-01-01T00:00:00Z', '2025-01-02T00:00:00Z')",
                [],
            )
            .unwrap();
        }

        // Now call list_jobs which triggers with_connection → migration
        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "old-job");
        assert!(!jobs[0].one_shot); // DEFAULT 0
    }

    #[test]
    fn add_job_sets_one_shot_false() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "*/5 * * * *", "echo recurring").unwrap();
        assert!(!job.one_shot);

        let listed = list_jobs(&config).unwrap();
        assert!(!listed[0].one_shot);
    }
}
