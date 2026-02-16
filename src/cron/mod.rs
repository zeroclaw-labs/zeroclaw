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
    /// If true, the job is paused and will not be picked up by the scheduler.
    pub paused: bool,
    /// If true, the job runs once and is then automatically removed.
    pub one_shot: bool,
}

#[allow(clippy::needless_pass_by_value)]
pub fn handle_command(command: super::CronCommands, config: &Config) -> Result<()> {
    match command {
        super::CronCommands::List => {
            let jobs = list_jobs(config)?;
            if jobs.is_empty() {
                println!("No scheduled tasks yet.");
                println!("\nUsage:");
                println!("  zeroclaw cron add '0 9 * * *' 'agent -m \"Good morning!\"'");
                println!("  zeroclaw cron once 30m 'echo reminder'");
                return Ok(());
            }

            println!("🕒 Scheduled jobs ({}):", jobs.len());
            for job in jobs {
                let last_run = job
                    .last_run
                    .map_or_else(|| "never".into(), |d| d.to_rfc3339());
                let last_status = job.last_status.unwrap_or_else(|| "n/a".into());
                let flags = match (job.paused, job.one_shot) {
                    (true, true) => " [paused, one-shot]",
                    (true, false) => " [paused]",
                    (false, true) => " [one-shot]",
                    (false, false) => "",
                };
                println!(
                    "- {} | {} | next={} | last={} ({}){}\n    cmd: {}",
                    job.id,
                    job.expression,
                    job.next_run.to_rfc3339(),
                    last_run,
                    last_status,
                    flags,
                    job.command
                );
            }
            Ok(())
        }
        super::CronCommands::Add {
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
        super::CronCommands::Once { delay, command } => {
            let job = add_once(config, &delay, &command)?;
            println!("✅ Added one-shot task {}", job.id);
            println!("  Runs at: {}", job.next_run.to_rfc3339());
            println!("  Cmd    : {}", job.command);
            Ok(())
        }
        super::CronCommands::Remove { id } => remove_job(config, &id),
        super::CronCommands::Pause { id } => {
            pause_job(config, &id)?;
            println!("⏸️  Paused job {id}");
            Ok(())
        }
        super::CronCommands::Resume { id } => {
            resume_job(config, &id)?;
            println!("▶️  Resumed job {id}");
            Ok(())
        }
    }
}

pub fn add_job(config: &Config, expression: &str, command: &str) -> Result<CronJob> {
    check_max_tasks(config)?;
    let now = Utc::now();
    let next_run = next_run_for(expression, now)?;
    let id = Uuid::new_v4().to_string();

    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO cron_jobs (id, expression, command, created_at, next_run, paused, one_shot)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0)",
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
        paused: false,
        one_shot: false,
    })
}

/// Add a one-shot delayed task. `delay` is a human-friendly duration like "30m", "2h", "1d".
pub fn add_once(config: &Config, delay: &str, command: &str) -> Result<CronJob> {
    check_max_tasks(config)?;
    let now = Utc::now();
    let duration = parse_duration(delay)?;
    let next_run = now + duration;
    let id = Uuid::new_v4().to_string();
    let expression = format!("@once:{delay}");

    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO cron_jobs (id, expression, command, created_at, next_run, paused, one_shot)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 1)",
            params![
                id,
                expression,
                command,
                now.to_rfc3339(),
                next_run.to_rfc3339()
            ],
        )
        .context("Failed to insert one-shot task")?;
        Ok(())
    })?;

    Ok(CronJob {
        id,
        expression,
        command: command.to_string(),
        next_run,
        last_run: None,
        last_status: None,
        paused: false,
        one_shot: true,
    })
}

/// Add a one-shot task at an absolute time.
pub fn add_once_at(config: &Config, at: DateTime<Utc>, command: &str) -> Result<CronJob> {
    check_max_tasks(config)?;
    let now = Utc::now();
    if at <= now {
        anyhow::bail!("Scheduled time must be in the future");
    }
    let id = Uuid::new_v4().to_string();
    let expression = format!("@at:{}", at.to_rfc3339());

    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO cron_jobs (id, expression, command, created_at, next_run, paused, one_shot)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 1)",
            params![
                id,
                expression,
                command,
                now.to_rfc3339(),
                at.to_rfc3339()
            ],
        )
        .context("Failed to insert one-shot task")?;
        Ok(())
    })?;

    Ok(CronJob {
        id,
        expression,
        command: command.to_string(),
        next_run: at,
        last_run: None,
        last_status: None,
        paused: false,
        one_shot: true,
    })
}

pub fn pause_job(config: &Config, id: &str) -> Result<()> {
    let changed = with_connection(config, |conn| {
        conn.execute(
            "UPDATE cron_jobs SET paused = 1 WHERE id = ?1",
            params![id],
        )
        .context("Failed to pause cron job")
    })?;
    if changed == 0 {
        anyhow::bail!("Cron job '{id}' not found");
    }
    Ok(())
}

pub fn resume_job(config: &Config, id: &str) -> Result<()> {
    let changed = with_connection(config, |conn| {
        conn.execute(
            "UPDATE cron_jobs SET paused = 0 WHERE id = ?1",
            params![id],
        )
        .context("Failed to resume cron job")
    })?;
    if changed == 0 {
        anyhow::bail!("Cron job '{id}' not found");
    }
    Ok(())
}

fn check_max_tasks(config: &Config) -> Result<()> {
    let count = with_connection(config, |conn| {
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM cron_jobs")?;
        let count: usize = stmt.query_row([], |row| row.get(0))?;
        Ok(count)
    })?;
    if count >= config.scheduler.max_tasks {
        anyhow::bail!(
            "Maximum number of scheduled tasks ({}) reached",
            config.scheduler.max_tasks
        );
    }
    Ok(())
}

fn parse_duration(s: &str) -> Result<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("Empty duration string");
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: i64 = num_str
        .parse()
        .with_context(|| format!("Invalid duration number: {num_str}"))?;
    match unit {
        "s" => Ok(chrono::Duration::seconds(num)),
        "m" => Ok(chrono::Duration::minutes(num)),
        "h" => Ok(chrono::Duration::hours(num)),
        "d" => Ok(chrono::Duration::days(num)),
        _ => anyhow::bail!("Unknown duration unit '{unit}', expected s/m/h/d"),
    }
}

pub fn list_jobs(config: &Config) -> Result<Vec<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, next_run, last_run, last_status, paused, one_shot
             FROM cron_jobs ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let next_run_raw: String = row.get(3)?;
            let last_run_raw: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                next_run_raw,
                last_run_raw,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
            ))
        })?;

        let mut jobs = Vec::new();
        for row in rows {
            let (id, expression, command, next_run_raw, last_run_raw, last_status, paused, one_shot) = row?;
            jobs.push(CronJob {
                id,
                expression,
                command,
                next_run: parse_rfc3339(&next_run_raw)?,
                last_run: match last_run_raw {
                    Some(raw) => Some(parse_rfc3339(&raw)?),
                    None => None,
                },
                last_status,
                paused,
                one_shot,
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
            "SELECT id, expression, command, next_run, last_run, last_status, paused, one_shot
             FROM cron_jobs WHERE next_run <= ?1 AND paused = 0 ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map(params![now.to_rfc3339()], |row| {
            let next_run_raw: String = row.get(3)?;
            let last_run_raw: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                next_run_raw,
                last_run_raw,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
            ))
        })?;

        let mut jobs = Vec::new();
        for row in rows {
            let (id, expression, command, next_run_raw, last_run_raw, last_status, paused, one_shot) = row?;
            jobs.push(CronJob {
                id,
                expression,
                command,
                next_run: parse_rfc3339(&next_run_raw)?,
                last_run: match last_run_raw {
                    Some(raw) => Some(parse_rfc3339(&raw)?),
                    None => None,
                },
                last_status,
                paused,
                one_shot,
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
    // One-shot tasks are removed after execution regardless of outcome.
    if job.one_shot {
        with_connection(config, |conn| {
            conn.execute("DELETE FROM cron_jobs WHERE id = ?1", params![job.id])
                .context("Failed to remove one-shot task after execution")?;
            Ok(())
        })?;
        return Ok(());
    }

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
            paused      INTEGER NOT NULL DEFAULT 0,
            one_shot    INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_cron_jobs_next_run ON cron_jobs(next_run);",
    )
    .context("Failed to initialize cron schema")?;

    // Migrate existing databases that lack the new columns.
    for col in &["paused", "one_shot"] {
        let alter = format!(
            "ALTER TABLE cron_jobs ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0"
        );
        // Ignore "duplicate column" errors from already-migrated DBs.
        let _ = conn.execute_batch(&alter);
    }

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
}
