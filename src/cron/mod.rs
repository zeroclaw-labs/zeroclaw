use crate::config::Config;
use crate::{aria, CronCommands};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use rusqlite::{params, Connection};
use std::str::FromStr;
use uuid::Uuid;

pub mod jobs_file;
pub mod scheduler;

#[derive(Debug, Clone)]
pub struct CronJob {
    pub id: String,
    pub expression: String,
    pub timezone: Option<String>,
    pub command: String,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

pub fn handle_command(command: super::CronCommands, config: Config) -> Result<()> {
    match command {
        CronCommands::List => {
            let jobs = list_jobs(&config)?;
            if jobs.is_empty() {
                println!("No scheduled tasks yet.");
                println!("\nUsage:");
                println!("  afw cron add '0 9 * * *' 'agent -m \"Good morning!\"'");
                return Ok(());
            }

            println!("🕒 Scheduled jobs ({}):", jobs.len());
            for job in jobs {
                let last_run = job
                    .last_run
                    .map_or_else(|| "never".into(), |d| d.to_rfc3339());
                let last_status = job.last_status.unwrap_or_else(|| "n/a".into());
                println!(
                    "- {} | {} [{}] | next={} | last={} ({})\n    cmd: {}",
                    job.id,
                    job.expression,
                    job.timezone.as_deref().unwrap_or("UTC"),
                    job.next_run.to_rfc3339(),
                    last_run,
                    last_status,
                    job.command
                );
            }
            Ok(())
        }
        CronCommands::SyncAria => {
            sync_from_aria(&config)?;
            println!("✅ Synced active aria_cron_functions into runtime cron jobs");
            Ok(())
        }
        CronCommands::Add {
            expression,
            command,
        } => {
            let job = add_job(&config, &expression, None, &command)?;
            println!("✅ Added cron job {}", job.id);
            println!("  Expr: {}", job.expression);
            println!("  Tz  : {}", job.timezone.as_deref().unwrap_or("UTC"));
            println!("  Next: {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            Ok(())
        }
        CronCommands::Remove { id } => remove_job(&config, &id),
    }
}

pub fn sync_from_aria(config: &Config) -> Result<()> {
    let aria_db = aria::db::AriaDb::open(&config.registry_db_path())?;
    let add_cfg = config.clone();
    let remove_cfg = config.clone();

    let add_job_cb: aria::cron_bridge::AddJobFn =
        std::sync::Arc::new(move |expr, timezone, command| {
            let job = add_job(&add_cfg, expr, timezone, command)?;
            Ok(aria::cron_bridge::CronJobHandle { id: job.id })
        });
    let remove_job_cb: aria::cron_bridge::RemoveJobFn =
        std::sync::Arc::new(move |job_id| remove_job(&remove_cfg, job_id));

    let bridge = aria::cron_bridge::CronBridge::new(aria_db, add_job_cb, remove_job_cb);
    bridge.sync_all()?;
    jobs_file::export_jobs_file(&config.workspace_dir)?;
    Ok(())
}

pub fn add_job(
    config: &Config,
    expression: &str,
    timezone: Option<&str>,
    command: &str,
) -> Result<CronJob> {
    let now = Utc::now();
    let next_run = next_run_for(expression, timezone, now)?;
    let id = Uuid::new_v4().to_string();

    with_connection(config, |conn| {
        conn.execute("DELETE FROM cron_jobs WHERE command = ?1", params![command])
            .context("Failed to clear existing cron job for command")?;
        conn.execute(
            "INSERT INTO cron_jobs (id, expression, timezone, command, created_at, next_run)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                expression,
                timezone,
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
        timezone: timezone.map(ToString::to_string),
        command: command.to_string(),
        next_run,
        last_run: None,
        last_status: None,
    })
}

pub fn list_jobs(config: &Config) -> Result<Vec<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, timezone, command, next_run, last_run, last_status
             FROM cron_jobs ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let next_run_raw: String = row.get(4)?;
            let last_run_raw: Option<String> = row.get(5)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                next_run_raw,
                last_run_raw,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;

        let mut jobs = Vec::new();
        for row in rows {
            let (id, expression, timezone, command, next_run_raw, last_run_raw, last_status) = row?;
            jobs.push(CronJob {
                id,
                expression,
                timezone,
                command,
                next_run: parse_rfc3339(&next_run_raw)?,
                last_run: match last_run_raw {
                    Some(raw) => Some(parse_rfc3339(&raw)?),
                    None => None,
                },
                last_status,
            });
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

    println!("✅ Removed cron job {id}");
    Ok(())
}

pub fn due_jobs(config: &Config, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, timezone, command, next_run, last_run, last_status
             FROM cron_jobs WHERE next_run <= ?1 ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map(params![now.to_rfc3339()], |row| {
            let next_run_raw: String = row.get(4)?;
            let last_run_raw: Option<String> = row.get(5)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                next_run_raw,
                last_run_raw,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;

        let mut jobs = Vec::new();
        for row in rows {
            let (id, expression, timezone, command, next_run_raw, last_run_raw, last_status) = row?;
            jobs.push(CronJob {
                id,
                expression,
                timezone,
                command,
                next_run: parse_rfc3339(&next_run_raw)?,
                last_run: match last_run_raw {
                    Some(raw) => Some(parse_rfc3339(&raw)?),
                    None => None,
                },
                last_status,
            });
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
    let next_run = next_run_for(&job.expression, job.timezone.as_deref(), now)?;
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

fn next_run_for(
    expression: &str,
    timezone: Option<&str>,
    from: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let normalized = normalize_expression(expression)?;
    let schedule = Schedule::from_str(&normalized)
        .with_context(|| format!("Invalid cron expression: {expression}"))?;
    if let Some(tz_name) = timezone {
        let tz: Tz = tz_name
            .parse()
            .with_context(|| format!("Invalid timezone for cron job: {tz_name}"))?;
        let from_tz = from.with_timezone(&tz);
        return schedule
            .after(&from_tz)
            .next()
            .map(|dt| dt.with_timezone(&Utc))
            .ok_or_else(|| anyhow::anyhow!("No future occurrence for expression: {expression}"));
    }

    schedule
        .after(&from)
        .next()
        .map(|dt| dt.with_timezone(&Utc))
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
            timezone    TEXT,
            command     TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            next_run    TEXT NOT NULL,
            last_run    TEXT,
            last_status TEXT,
            last_output TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_cron_jobs_next_run ON cron_jobs(next_run);",
    )
    .context("Failed to initialize cron schema")?;
    let _ = conn.execute("ALTER TABLE cron_jobs ADD COLUMN timezone TEXT", []);

    f(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config::default();
        config.workspace_dir = tmp.path().join("workspace");
        config.config_path = tmp.path().join("config.toml");
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    #[test]
    fn add_job_accepts_five_field_expression() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "*/5 * * * *", None, "echo ok").unwrap();

        assert_eq!(job.expression, "*/5 * * * *");
        assert_eq!(job.command, "echo ok");
    }

    #[test]
    fn add_job_rejects_invalid_field_count() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let err = add_job(&config, "* * * *", None, "echo bad").unwrap_err();
        assert!(err.to_string().contains("expected 5, 6, or 7 fields"));
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "*/10 * * * *", None, "echo roundtrip").unwrap();
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

        let _job = add_job(&config, "* * * * *", None, "echo due").unwrap();

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

        let job = add_job(&config, "*/15 * * * *", None, "echo run").unwrap();
        reschedule_after_run(&config, &job, false, "failed output").unwrap();

        let listed = list_jobs(&config).unwrap();
        let stored = listed.iter().find(|j| j.id == job.id).unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert!(stored.last_run.is_some());
    }
}
