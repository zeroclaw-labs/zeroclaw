use crate::cron::{
    CronJob, CronJobPatch, CronRun, DeliveryConfig, JobType, Schedule, SessionTarget,
    next_run_for_schedule, schedule_cron_expression, validate_delivery_config, validate_schedule,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::types::{FromSqlResult, ValueRef};
use rusqlite::{Connection, params};
use uuid::Uuid;
use zeroclaw_config::schema::Config;

const MAX_CRON_OUTPUT_BYTES: usize = 16 * 1024;
const TRUNCATED_OUTPUT_MARKER: &str = "\n...[truncated]";

impl rusqlite::types::FromSql for JobType {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        JobType::try_from(text).map_err(|e| rusqlite::types::FromSqlError::Other(e.into()))
    }
}

#[cfg(test)]
pub fn add_job(config: &Config, expression: &str, command: &str) -> Result<CronJob> {
    let schedule = Schedule::Cron {
        expr: expression.to_string(),
        tz: None,
    };
    add_shell_job(config, None, schedule, command, None)
}

pub fn add_shell_job(
    config: &Config,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
    delivery: Option<DeliveryConfig>,
) -> Result<CronJob> {
    let now = Utc::now();
    validate_schedule(&schedule, now)?;
    validate_delivery_config(delivery.as_ref())?;
    let next_run = next_run_for_schedule(&schedule, now)?;
    let id = Uuid::new_v4().to_string();
    let expression = schedule_cron_expression(&schedule).unwrap_or_default();
    let schedule_json = serde_json::to_string(&schedule)?;
    let delivery = delivery.unwrap_or_default();

    let delete_after_run = matches!(schedule, Schedule::At { .. });

    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO cron_jobs (
                id, expression, command, schedule, job_type, prompt, name, session_target, model,
                enabled, delivery, delete_after_run, created_at, next_run
             ) VALUES (?1, ?2, ?3, ?4, 'shell', NULL, ?5, 'isolated', NULL, 1, ?6, ?7, ?8, ?9)",
            params![
                id,
                expression,
                command,
                schedule_json,
                name,
                serde_json::to_string(&delivery)?,
                if delete_after_run { 1 } else { 0 },
                now.to_rfc3339(),
                next_run.to_rfc3339(),
            ],
        )
        .context("Failed to insert cron shell job")?;
        Ok(())
    })?;

    get_job(config, &id)
}

#[allow(clippy::too_many_arguments)]
pub fn add_agent_job(
    config: &Config,
    name: Option<String>,
    schedule: Schedule,
    prompt: &str,
    session_target: SessionTarget,
    model: Option<String>,
    delivery: Option<DeliveryConfig>,
    delete_after_run: bool,
    allowed_tools: Option<Vec<String>>,
) -> Result<CronJob> {
    let now = Utc::now();
    validate_schedule(&schedule, now)?;
    validate_delivery_config(delivery.as_ref())?;
    let next_run = next_run_for_schedule(&schedule, now)?;
    let id = Uuid::new_v4().to_string();
    let expression = schedule_cron_expression(&schedule).unwrap_or_default();
    let schedule_json = serde_json::to_string(&schedule)?;
    let delivery = delivery.unwrap_or_default();

    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO cron_jobs (
                id, expression, command, schedule, job_type, prompt, name, session_target, model,
                enabled, delivery, delete_after_run, allowed_tools, created_at, next_run
             ) VALUES (?1, ?2, '', ?3, 'agent', ?4, ?5, ?6, ?7, 1, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                expression,
                schedule_json,
                prompt,
                name,
                session_target.as_str(),
                model,
                serde_json::to_string(&delivery)?,
                if delete_after_run { 1 } else { 0 },
                encode_allowed_tools(allowed_tools.as_ref())?,
                now.to_rfc3339(),
                next_run.to_rfc3339(),
            ],
        )
        .context("Failed to insert cron agent job")?;
        Ok(())
    })?;

    get_job(config, &id)
}

pub fn list_jobs(config: &Config) -> Result<Vec<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, schedule, job_type, prompt, name, session_target, model,
                    enabled, delivery, delete_after_run, created_at, next_run, last_run, last_status, last_output,
                    allowed_tools, source, uses_memory
             FROM cron_jobs ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map([], map_cron_job_row)?;

        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row?);
        }
        Ok(jobs)
    })
}

pub fn get_job(config: &Config, job_id: &str) -> Result<CronJob> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, schedule, job_type, prompt, name, session_target, model,
                    enabled, delivery, delete_after_run, created_at, next_run, last_run, last_status, last_output,
                    allowed_tools, source, uses_memory
             FROM cron_jobs WHERE id = ?1",
        )?;

        let mut rows = stmt.query(params![job_id])?;
        if let Some(row) = rows.next()? {
            map_cron_job_row(row).map_err(Into::into)
        } else {
            anyhow::bail!("Cron job '{job_id}' not found")
        }
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
    let lim = i64::try_from(config.scheduler.max_tasks.max(1))
        .context("Scheduler max_tasks overflows i64")?;
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, schedule, job_type, prompt, name, session_target, model,
                    enabled, delivery, delete_after_run, created_at, next_run, last_run, last_status, last_output,
                    allowed_tools, source, uses_memory
             FROM cron_jobs
             WHERE enabled = 1 AND next_run <= ?1
             ORDER BY next_run ASC
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(params![now.to_rfc3339(), lim], map_cron_job_row)?;

        let mut jobs = Vec::new();
        for row in rows {
            match row {
                Ok(job) => jobs.push(job),
                Err(e) => tracing::warn!("Skipping cron job with unparseable row data: {e}"),
            }
        }
        Ok(jobs)
    })
}

/// Return **all** enabled overdue jobs without the `max_tasks` limit.
///
/// Used by the scheduler startup catch-up to ensure every missed job is
/// executed at least once after a period of downtime (late boot, daemon
/// restart, etc.).
pub fn all_overdue_jobs(config: &Config, now: DateTime<Utc>) -> Result<Vec<CronJob>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, expression, command, schedule, job_type, prompt, name, session_target, model,
                    enabled, delivery, delete_after_run, created_at, next_run, last_run, last_status, last_output,
                    allowed_tools, source, uses_memory
             FROM cron_jobs
             WHERE enabled = 1 AND next_run <= ?1
             ORDER BY next_run ASC",
        )?;

        let rows = stmt.query_map(params![now.to_rfc3339()], map_cron_job_row)?;

        let mut jobs = Vec::new();
        for row in rows {
            match row {
                Ok(job) => jobs.push(job),
                Err(e) => tracing::warn!("Skipping cron job with unparseable row data: {e}"),
            }
        }
        Ok(jobs)
    })
}

pub fn update_job(config: &Config, job_id: &str, patch: CronJobPatch) -> Result<CronJob> {
    let mut job = get_job(config, job_id)?;
    let mut schedule_changed = false;

    if let Some(schedule) = patch.schedule {
        validate_schedule(&schedule, Utc::now())?;
        job.schedule = schedule;
        job.expression = schedule_cron_expression(&job.schedule).unwrap_or_default();
        schedule_changed = true;
    }
    if let Some(command) = patch.command {
        job.command = command;
    }
    if let Some(prompt) = patch.prompt {
        job.prompt = Some(prompt);
    }
    if let Some(name) = patch.name {
        job.name = Some(name);
    }
    if let Some(enabled) = patch.enabled {
        job.enabled = enabled;
    }
    if let Some(delivery) = patch.delivery {
        job.delivery = delivery;
    }
    if let Some(model) = patch.model {
        job.model = Some(model);
    }
    if let Some(target) = patch.session_target {
        job.session_target = target;
    }
    if let Some(delete_after_run) = patch.delete_after_run {
        job.delete_after_run = delete_after_run;
    }
    if let Some(allowed_tools) = patch.allowed_tools {
        // Empty list means "clear the allowlist" (all tools available),
        // not "allow zero tools".
        if allowed_tools.is_empty() {
            job.allowed_tools = None;
        } else {
            job.allowed_tools = Some(allowed_tools);
        }
    }
    if let Some(uses_memory) = patch.uses_memory {
        job.uses_memory = uses_memory;
    }

    if schedule_changed {
        job.next_run = next_run_for_schedule(&job.schedule, Utc::now())?;
    }

    with_connection(config, |conn| {
        conn.execute(
            "UPDATE cron_jobs
             SET expression = ?1, command = ?2, schedule = ?3, job_type = ?4, prompt = ?5, name = ?6,
                 session_target = ?7, model = ?8, enabled = ?9, delivery = ?10, delete_after_run = ?11,
                 allowed_tools = ?12, next_run = ?13, uses_memory = ?14
             WHERE id = ?15",
            params![
                job.expression,
                job.command,
                serde_json::to_string(&job.schedule)?,
                <JobType as Into<&str>>::into(job.job_type).to_string(),
                job.prompt,
                job.name,
                job.session_target.as_str(),
                job.model,
                if job.enabled { 1 } else { 0 },
                serde_json::to_string(&job.delivery)?,
                if job.delete_after_run { 1 } else { 0 },
                encode_allowed_tools(job.allowed_tools.as_ref())?,
                job.next_run.to_rfc3339(),
                if job.uses_memory { 1 } else { 0 },
                job.id,
            ],
        )
        .context("Failed to update cron job")?;
        Ok(())
    })?;

    get_job(config, job_id)
}

pub fn record_last_run(
    config: &Config,
    job_id: &str,
    finished_at: DateTime<Utc>,
    success: bool,
    output: &str,
) -> Result<()> {
    let status = if success { "ok" } else { "error" };
    record_last_run_with_status(config, job_id, finished_at, status, output)
}

pub fn record_last_run_with_status(
    config: &Config,
    job_id: &str,
    finished_at: DateTime<Utc>,
    status: &str,
    output: &str,
) -> Result<()> {
    let bounded_output = truncate_cron_output(output);
    with_connection(config, |conn| {
        conn.execute(
            "UPDATE cron_jobs
             SET last_run = ?1, last_status = ?2, last_output = ?3
             WHERE id = ?4",
            params![finished_at.to_rfc3339(), status, bounded_output, job_id],
        )
        .context("Failed to update cron last run fields")?;
        Ok(())
    })
}

pub fn reschedule_after_run(
    config: &Config,
    job: &CronJob,
    success: bool,
    output: &str,
) -> Result<()> {
    let status = if success { "ok" } else { "error" };
    reschedule_after_run_with_status(config, job, status, output)
}

pub fn reschedule_after_run_with_status(
    config: &Config,
    job: &CronJob,
    status: &str,
    output: &str,
) -> Result<()> {
    let now = Utc::now();
    let bounded_output = truncate_cron_output(output);

    // One-shot `At` schedules have no future occurrence — record the run
    // result and disable the job so it won't be picked up again.
    if matches!(job.schedule, Schedule::At { .. }) {
        with_connection(config, |conn| {
            conn.execute(
                "UPDATE cron_jobs
                 SET enabled = 0, last_run = ?1, last_status = ?2, last_output = ?3
                 WHERE id = ?4",
                params![now.to_rfc3339(), status, bounded_output, job.id],
            )
            .context("Failed to disable completed one-shot cron job")?;
            Ok(())
        })
    } else {
        let next_run = next_run_for_schedule(&job.schedule, now)?;
        with_connection(config, |conn| {
            conn.execute(
                "UPDATE cron_jobs
                 SET next_run = ?1, last_run = ?2, last_status = ?3, last_output = ?4
                 WHERE id = ?5",
                params![
                    next_run.to_rfc3339(),
                    now.to_rfc3339(),
                    status,
                    bounded_output,
                    job.id
                ],
            )
            .context("Failed to update cron job run state")?;
            Ok(())
        })
    }
}

pub fn record_run(
    config: &Config,
    job_id: &str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    status: &str,
    output: Option<&str>,
    duration_ms: i64,
) -> Result<()> {
    let bounded_output = output.map(truncate_cron_output);
    with_connection(config, |conn| {
        // Wrap INSERT + pruning DELETE in an explicit transaction so that
        // if the DELETE fails, the INSERT is rolled back and the run table
        // cannot grow unboundedly.
        let tx = conn.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO cron_runs (job_id, started_at, finished_at, status, output, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                job_id,
                started_at.to_rfc3339(),
                finished_at.to_rfc3339(),
                status,
                bounded_output.as_deref(),
                duration_ms,
            ],
        )
        .context("Failed to insert cron run")?;

        let keep = i64::from(config.cron.max_run_history.max(1));
        tx.execute(
            "DELETE FROM cron_runs
             WHERE job_id = ?1
               AND id NOT IN (
                 SELECT id FROM cron_runs
                 WHERE job_id = ?1
                 ORDER BY started_at DESC, id DESC
                 LIMIT ?2
               )",
            params![job_id, keep],
        )
        .context("Failed to prune cron run history")?;

        tx.commit()
            .context("Failed to commit cron run transaction")?;
        Ok(())
    })
}

fn truncate_cron_output(output: &str) -> String {
    if output.len() <= MAX_CRON_OUTPUT_BYTES {
        return output.to_string();
    }

    if MAX_CRON_OUTPUT_BYTES <= TRUNCATED_OUTPUT_MARKER.len() {
        return TRUNCATED_OUTPUT_MARKER.to_string();
    }

    let mut cutoff = MAX_CRON_OUTPUT_BYTES - TRUNCATED_OUTPUT_MARKER.len();
    while cutoff > 0 && !output.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let mut truncated = output[..cutoff].to_string();
    truncated.push_str(TRUNCATED_OUTPUT_MARKER);
    truncated
}

pub fn list_runs(config: &Config, job_id: &str, limit: usize) -> Result<Vec<CronRun>> {
    with_connection(config, |conn| {
        let lim = i64::try_from(limit.max(1)).context("Run history limit overflow")?;
        let mut stmt = conn.prepare(
            "SELECT id, job_id, started_at, finished_at, status, output, duration_ms
             FROM cron_runs
             WHERE job_id = ?1
             ORDER BY started_at DESC, id DESC
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(params![job_id, lim], |row| {
            Ok(CronRun {
                id: row.get(0)?,
                job_id: row.get(1)?,
                started_at: parse_rfc3339(&row.get::<_, String>(2)?)
                    .map_err(sql_conversion_error)?,
                finished_at: parse_rfc3339(&row.get::<_, String>(3)?)
                    .map_err(sql_conversion_error)?,
                status: row.get(4)?,
                output: row.get(5)?,
                duration_ms: row.get(6)?,
            })
        })?;

        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    })
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("Invalid RFC3339 timestamp in cron DB: {raw}"))?;
    Ok(parsed.with_timezone(&Utc))
}

fn sql_conversion_error(err: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(err.into())
}

fn map_cron_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CronJob> {
    let expression: String = row.get(1)?;
    let schedule_raw: Option<String> = row.get(3)?;
    let schedule =
        decode_schedule(schedule_raw.as_deref(), &expression).map_err(sql_conversion_error)?;

    let delivery_raw: Option<String> = row.get(10)?;
    let delivery = decode_delivery(delivery_raw.as_deref()).map_err(sql_conversion_error)?;

    let next_run_raw: String = row.get(13)?;
    let last_run_raw: Option<String> = row.get(14)?;
    let created_at_raw: String = row.get(12)?;
    let allowed_tools_raw: Option<String> = row.get(17)?;
    let source: Option<String> = row.get(18)?;
    let uses_memory: Option<i64> = row.get(19)?;

    Ok(CronJob {
        id: row.get(0)?,
        expression,
        schedule,
        command: row.get(2)?,
        job_type: row.get(4)?,
        prompt: row.get(5)?,
        name: row.get(6)?,
        session_target: SessionTarget::parse(&row.get::<_, String>(7)?),
        model: row.get(8)?,
        enabled: row.get::<_, i64>(9)? != 0,
        delivery,
        delete_after_run: row.get::<_, i64>(11)? != 0,
        source: source.unwrap_or_else(|| "imperative".to_string()),
        uses_memory: uses_memory != Some(0),
        created_at: parse_rfc3339(&created_at_raw).map_err(sql_conversion_error)?,
        next_run: parse_rfc3339(&next_run_raw).map_err(sql_conversion_error)?,
        last_run: match last_run_raw {
            Some(raw) => Some(parse_rfc3339(&raw).map_err(sql_conversion_error)?),
            None => None,
        },
        last_status: row.get(15)?,
        last_output: row.get(16)?,
        allowed_tools: decode_allowed_tools(allowed_tools_raw.as_deref())
            .map_err(sql_conversion_error)?,
    })
}

fn decode_schedule(schedule_raw: Option<&str>, expression: &str) -> Result<Schedule> {
    if let Some(raw) = schedule_raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return serde_json::from_str(trimmed)
                .with_context(|| format!("Failed to parse cron schedule JSON: {trimmed}"));
        }
    }

    if expression.trim().is_empty() {
        anyhow::bail!("Missing schedule and legacy expression for cron job")
    }

    Ok(Schedule::Cron {
        expr: expression.to_string(),
        tz: None,
    })
}

fn decode_delivery(delivery_raw: Option<&str>) -> Result<DeliveryConfig> {
    if let Some(raw) = delivery_raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return serde_json::from_str(trimmed)
                .with_context(|| format!("Failed to parse cron delivery JSON: {trimmed}"));
        }
    }
    Ok(DeliveryConfig::default())
}

fn encode_allowed_tools(allowed_tools: Option<&Vec<String>>) -> Result<Option<String>> {
    allowed_tools
        .map(serde_json::to_string)
        .transpose()
        .context("Failed to serialize cron allowed_tools")
}

fn decode_allowed_tools(raw: Option<&str>) -> Result<Option<Vec<String>>> {
    if let Some(raw) = raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return serde_json::from_str(trimmed)
                .map(Some)
                .with_context(|| format!("Failed to parse cron allowed_tools JSON: {trimmed}"));
        }
    }
    Ok(None)
}

/// Synchronize declarative cron job definitions from config into the database.
///
/// For each declarative job (identified by `id`):
/// - If the job exists in DB: update it to match the config definition.
/// - If the job does not exist: insert it.
///
/// Jobs created imperatively (via CLI/API) are never modified or deleted.
/// Declarative jobs that are no longer present in config are removed.
pub fn sync_declarative_jobs(
    config: &Config,
    decls: &[zeroclaw_config::schema::CronJobDecl],
) -> Result<()> {
    use zeroclaw_config::schema::CronScheduleDecl;

    if decls.is_empty() {
        // If no declarative jobs are defined, clean up any previously
        // synced declarative jobs that are no longer in config.
        with_connection(config, |conn| {
            let deleted = conn
                .execute("DELETE FROM cron_jobs WHERE source = 'declarative'", [])
                .context("Failed to remove stale declarative cron jobs")?;
            if deleted > 0 {
                tracing::info!(
                    count = deleted,
                    "Removed declarative cron jobs no longer in config"
                );
            }
            Ok(())
        })?;
        return Ok(());
    }

    // Validate declarations before touching the DB.
    for decl in decls {
        validate_decl(decl)?;
    }

    let now = Utc::now();

    with_connection(config, |conn| {
        // Collect IDs of all declarative jobs currently defined in config.
        let config_ids: std::collections::HashSet<&str> =
            decls.iter().map(|d| d.id.as_str()).collect();

        // Remove declarative jobs no longer in config.
        {
            let mut stmt = conn.prepare("SELECT id FROM cron_jobs WHERE source = 'declarative'")?;
            let db_ids: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            for db_id in &db_ids {
                if !config_ids.contains(db_id.as_str()) {
                    conn.execute("DELETE FROM cron_jobs WHERE id = ?1", params![db_id])
                        .with_context(|| {
                            format!("Failed to remove stale declarative cron job '{db_id}'")
                        })?;
                    tracing::info!(
                        job_id = %db_id,
                        "Removed declarative cron job no longer in config"
                    );
                }
            }
        }

        for decl in decls {
            let schedule = convert_schedule_decl(&decl.schedule)?;
            let expression = schedule_cron_expression(&schedule).unwrap_or_default();
            let schedule_json = serde_json::to_string(&schedule)?;
            let job_type = &decl.job_type;
            let session_target = decl.session_target.as_deref().unwrap_or("isolated");
            let delivery = match &decl.delivery {
                Some(d) => convert_delivery_decl(d),
                None => DeliveryConfig::default(),
            };
            let delivery_json = serde_json::to_string(&delivery)?;
            let allowed_tools_json = encode_allowed_tools(decl.allowed_tools.as_ref())?;
            let command = decl.command.as_deref().unwrap_or("");
            let delete_after_run = matches!(decl.schedule, CronScheduleDecl::At { .. });

            // Check if job already exists.
            let exists: bool = conn
                .prepare("SELECT COUNT(*) FROM cron_jobs WHERE id = ?1")?
                .query_row(params![decl.id], |row| row.get::<_, i64>(0))
                .map(|c| c > 0)
                .unwrap_or(false);

            if exists {
                // Update existing declarative job — preserve runtime state
                // (next_run, last_run, last_status, last_output, created_at).
                // Only update the schedule's next_run if the schedule itself changed.
                let current_schedule_raw: Option<String> = conn
                    .prepare("SELECT schedule FROM cron_jobs WHERE id = ?1")?
                    .query_row(params![decl.id], |row| row.get(0))
                    .ok();

                let schedule_changed = current_schedule_raw.as_deref() != Some(&schedule_json);

                if schedule_changed {
                    let next_run = next_run_for_schedule(&schedule, now)?;
                    conn.execute(
                        "UPDATE cron_jobs
                         SET expression = ?1, command = ?2, schedule = ?3, job_type = ?4,
                             prompt = ?5, name = ?6, session_target = ?7, model = ?8,
                             enabled = ?9, delivery = ?10, delete_after_run = ?11,
                             allowed_tools = ?12, source = 'declarative', next_run = ?13,
                             uses_memory = ?14
                         WHERE id = ?15",
                        params![
                            expression,
                            command,
                            schedule_json,
                            job_type,
                            decl.prompt,
                            decl.name,
                            session_target,
                            decl.model,
                            if decl.enabled { 1 } else { 0 },
                            delivery_json,
                            if delete_after_run { 1 } else { 0 },
                            allowed_tools_json,
                            next_run.to_rfc3339(),
                            if decl.uses_memory { 1 } else { 0 },
                            decl.id,
                        ],
                    )
                    .with_context(|| {
                        format!("Failed to update declarative cron job '{}'", decl.id)
                    })?;
                } else {
                    conn.execute(
                        "UPDATE cron_jobs
                         SET expression = ?1, command = ?2, schedule = ?3, job_type = ?4,
                             prompt = ?5, name = ?6, session_target = ?7, model = ?8,
                             enabled = ?9, delivery = ?10, delete_after_run = ?11,
                             allowed_tools = ?12, source = 'declarative',
                             uses_memory = ?13
                         WHERE id = ?14",
                        params![
                            expression,
                            command,
                            schedule_json,
                            job_type,
                            decl.prompt,
                            decl.name,
                            session_target,
                            decl.model,
                            if decl.enabled { 1 } else { 0 },
                            delivery_json,
                            if delete_after_run { 1 } else { 0 },
                            allowed_tools_json,
                            if decl.uses_memory { 1 } else { 0 },
                            decl.id,
                        ],
                    )
                    .with_context(|| {
                        format!("Failed to update declarative cron job '{}'", decl.id)
                    })?;
                }

                tracing::debug!(job_id = %decl.id, "Updated declarative cron job");
            } else {
                // Insert new declarative job.
                let next_run = next_run_for_schedule(&schedule, now)?;
                conn.execute(
                    "INSERT INTO cron_jobs (
                        id, expression, command, schedule, job_type, prompt, name,
                        session_target, model, enabled, delivery, delete_after_run,
                        allowed_tools, source, uses_memory, created_at, next_run
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'declarative', ?14, ?15, ?16)",
                    params![
                        decl.id,
                        expression,
                        command,
                        schedule_json,
                        job_type,
                        decl.prompt,
                        decl.name,
                        session_target,
                        decl.model,
                        if decl.enabled { 1 } else { 0 },
                        delivery_json,
                        if delete_after_run { 1 } else { 0 },
                        allowed_tools_json,
                        if decl.uses_memory { 1 } else { 0 },
                        now.to_rfc3339(),
                        next_run.to_rfc3339(),
                    ],
                )
                .with_context(|| {
                    format!(
                        "Failed to insert declarative cron job '{}'",
                        decl.id
                    )
                })?;

                tracing::info!(job_id = %decl.id, "Inserted declarative cron job from config");
            }
        }

        Ok(())
    })
}

/// Validate a declarative cron job definition.
fn validate_decl(decl: &zeroclaw_config::schema::CronJobDecl) -> Result<()> {
    if decl.id.trim().is_empty() {
        anyhow::bail!("Declarative cron job has empty id");
    }

    match decl.job_type.to_lowercase().as_str() {
        "shell" => {
            if decl.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                anyhow::bail!(
                    "Declarative cron job '{}': shell job requires a non-empty 'command'",
                    decl.id
                );
            }
        }
        "agent" => {
            if decl.prompt.as_deref().is_none_or(|p| p.trim().is_empty()) {
                anyhow::bail!(
                    "Declarative cron job '{}': agent job requires a non-empty 'prompt'",
                    decl.id
                );
            }
        }
        other => {
            anyhow::bail!(
                "Declarative cron job '{}': invalid job_type '{}', expected 'shell' or 'agent'",
                decl.id,
                other
            );
        }
    }

    Ok(())
}

/// Convert a `CronScheduleDecl` to the runtime `Schedule` type.
fn convert_schedule_decl(decl: &zeroclaw_config::schema::CronScheduleDecl) -> Result<Schedule> {
    use zeroclaw_config::schema::CronScheduleDecl;
    match decl {
        CronScheduleDecl::Cron { expr, tz } => Ok(Schedule::Cron {
            expr: expr.clone(),
            tz: tz.clone(),
        }),
        CronScheduleDecl::Every { every_ms } => Ok(Schedule::Every {
            every_ms: *every_ms,
        }),
        CronScheduleDecl::At { at } => {
            let parsed = DateTime::parse_from_rfc3339(at)
                .with_context(|| {
                    format!("Invalid RFC3339 timestamp in declarative cron 'at': {at}")
                })?
                .with_timezone(&Utc);
            Ok(Schedule::At { at: parsed })
        }
    }
}

/// Convert a `DeliveryConfigDecl` to the runtime `DeliveryConfig`.
fn convert_delivery_decl(decl: &zeroclaw_config::schema::DeliveryConfigDecl) -> DeliveryConfig {
    DeliveryConfig {
        mode: decl.mode.clone(),
        channel: decl.channel.clone(),
        to: decl.to.clone(),
        best_effort: decl.best_effort,
    }
}

fn add_column_if_missing(conn: &Connection, name: &str, sql_type: &str) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(cron_jobs)")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let col_name: String = row.get(1)?;
        if col_name == name {
            return Ok(());
        }
    }
    // Drop the statement/rows before executing ALTER to release any locks
    drop(rows);
    drop(stmt);

    // Tolerate "duplicate column name" errors to handle the race where
    // another process adds the column between our PRAGMA check and ALTER.
    match conn.execute(
        &format!("ALTER TABLE cron_jobs ADD COLUMN {name} {sql_type}"),
        [],
    ) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(err, Some(ref msg)))
            if msg.contains("duplicate column name") =>
        {
            tracing::debug!("Column cron_jobs.{name} already exists (concurrent migration): {err}");
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("Failed to add cron_jobs.{name}")),
    }
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
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS cron_jobs (
            id               TEXT PRIMARY KEY,
            expression       TEXT NOT NULL,
            command          TEXT NOT NULL,
            schedule         TEXT,
            job_type         TEXT NOT NULL DEFAULT 'shell',
            prompt           TEXT,
            name             TEXT,
            session_target   TEXT NOT NULL DEFAULT 'isolated',
            model            TEXT,
            enabled          INTEGER NOT NULL DEFAULT 1,
            delivery         TEXT,
            delete_after_run INTEGER NOT NULL DEFAULT 0,
            allowed_tools    TEXT,
            created_at       TEXT NOT NULL,
            next_run         TEXT NOT NULL,
            last_run         TEXT,
            last_status      TEXT,
            last_output      TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_cron_jobs_next_run ON cron_jobs(next_run);

        CREATE TABLE IF NOT EXISTS cron_runs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id      TEXT NOT NULL,
            started_at  TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            status      TEXT NOT NULL,
            output      TEXT,
            duration_ms INTEGER,
            FOREIGN KEY (job_id) REFERENCES cron_jobs(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_cron_runs_job_id ON cron_runs(job_id);
        CREATE INDEX IF NOT EXISTS idx_cron_runs_started_at ON cron_runs(started_at);
        CREATE INDEX IF NOT EXISTS idx_cron_runs_job_started ON cron_runs(job_id, started_at);",
    )
    .context("Failed to initialize cron schema")?;

    add_column_if_missing(&conn, "schedule", "TEXT")?;
    add_column_if_missing(&conn, "job_type", "TEXT NOT NULL DEFAULT 'shell'")?;
    add_column_if_missing(&conn, "prompt", "TEXT")?;
    add_column_if_missing(&conn, "name", "TEXT")?;
    add_column_if_missing(&conn, "session_target", "TEXT NOT NULL DEFAULT 'isolated'")?;
    add_column_if_missing(&conn, "model", "TEXT")?;
    add_column_if_missing(&conn, "enabled", "INTEGER NOT NULL DEFAULT 1")?;
    add_column_if_missing(&conn, "delivery", "TEXT")?;
    add_column_if_missing(&conn, "delete_after_run", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(&conn, "allowed_tools", "TEXT")?;
    add_column_if_missing(&conn, "source", "TEXT DEFAULT 'imperative'")?;
    add_column_if_missing(&conn, "uses_memory", "INTEGER NOT NULL DEFAULT 1")?;

    f(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;
    use zeroclaw_config::schema::Config;

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
        assert!(matches!(job.schedule, Schedule::Cron { .. }));
    }

    #[test]
    fn add_shell_job_marks_at_schedule_for_auto_delete() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let one_shot = add_shell_job(
            &config,
            None,
            Schedule::At {
                at: Utc::now() + ChronoDuration::minutes(10),
            },
            "echo once",
            None,
        )
        .unwrap();
        assert!(one_shot.delete_after_run);

        let recurring = add_shell_job(
            &config,
            None,
            Schedule::Every { every_ms: 60_000 },
            "echo recurring",
            None,
        )
        .unwrap();
        assert!(!recurring.delete_after_run);
    }

    #[test]
    fn add_shell_job_persists_delivery() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_shell_job(
            &config,
            Some("deliver-shell".into()),
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo delivered",
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("discord".into()),
                to: Some("1234567890".into()),
                best_effort: true,
            }),
        )
        .unwrap();

        assert_eq!(job.delivery.mode, "announce");
        assert_eq!(job.delivery.channel.as_deref(), Some("discord"));
        assert_eq!(job.delivery.to.as_deref(), Some("1234567890"));

        let stored = get_job(&config, &job.id).unwrap();
        assert_eq!(stored.delivery.mode, "announce");
        assert_eq!(stored.delivery.channel.as_deref(), Some("discord"));
        assert_eq!(stored.delivery.to.as_deref(), Some("1234567890"));
    }

    #[test]
    fn add_agent_job_rejects_invalid_announce_delivery() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let err = add_agent_job(
            &config,
            Some("deliver-agent".into()),
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "summarize logs",
            SessionTarget::Isolated,
            None,
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("discord".into()),
                to: None,
                best_effort: true,
            }),
            false,
            None,
        )
        .unwrap_err();

        assert!(err.to_string().contains("delivery.to is required"));
    }

    #[test]
    fn add_shell_job_rejects_invalid_delivery_mode() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let err = add_shell_job(
            &config,
            Some("deliver-shell".into()),
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo delivered",
            Some(DeliveryConfig {
                mode: "annouce".into(),
                channel: Some("discord".into()),
                to: Some("1234567890".into()),
                best_effort: true,
            }),
        )
        .unwrap_err();

        assert!(err.to_string().contains("unsupported delivery mode"));
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
    fn due_jobs_filters_by_timestamp_and_enabled() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "* * * * *", "echo due").unwrap();

        let due_now = due_jobs(&config, Utc::now()).unwrap();
        assert!(due_now.is_empty(), "new job should not be due immediately");

        let far_future = Utc::now() + ChronoDuration::days(365);
        let due_future = due_jobs(&config, far_future).unwrap();
        assert_eq!(due_future.len(), 1, "job should be due in far future");

        let _ = update_job(
            &config,
            &job.id,
            CronJobPatch {
                enabled: Some(false),
                ..CronJobPatch::default()
            },
        )
        .unwrap();
        let due_after_disable = due_jobs(&config, far_future).unwrap();
        assert!(due_after_disable.is_empty());
    }

    #[test]
    fn due_jobs_respects_scheduler_max_tasks_limit() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.scheduler.max_tasks = 2;

        let _ = add_job(&config, "* * * * *", "echo due-1").unwrap();
        let _ = add_job(&config, "* * * * *", "echo due-2").unwrap();
        let _ = add_job(&config, "* * * * *", "echo due-3").unwrap();

        let far_future = Utc::now() + ChronoDuration::days(365);
        let due = due_jobs(&config, far_future).unwrap();
        assert_eq!(due.len(), 2);
    }

    #[test]
    fn all_overdue_jobs_ignores_max_tasks_limit() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.scheduler.max_tasks = 2;

        let _ = add_job(&config, "* * * * *", "echo ov-1").unwrap();
        let _ = add_job(&config, "* * * * *", "echo ov-2").unwrap();
        let _ = add_job(&config, "* * * * *", "echo ov-3").unwrap();

        let far_future = Utc::now() + ChronoDuration::days(365);
        // due_jobs respects the limit
        let due = due_jobs(&config, far_future).unwrap();
        assert_eq!(due.len(), 2);
        // all_overdue_jobs returns everything
        let overdue = all_overdue_jobs(&config, far_future).unwrap();
        assert_eq!(overdue.len(), 3);
    }

    #[test]
    fn all_overdue_jobs_excludes_disabled_jobs() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_job(&config, "* * * * *", "echo disabled").unwrap();
        let _ = update_job(
            &config,
            &job.id,
            CronJobPatch {
                enabled: Some(false),
                ..CronJobPatch::default()
            },
        )
        .unwrap();

        let far_future = Utc::now() + ChronoDuration::days(365);
        let overdue = all_overdue_jobs(&config, far_future).unwrap();
        assert!(overdue.is_empty());
    }

    #[test]
    fn add_agent_job_persists_allowed_tools() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_agent_job(
            &config,
            Some("agent".into()),
            Schedule::Every { every_ms: 60_000 },
            "do work",
            SessionTarget::Isolated,
            None,
            None,
            false,
            Some(vec!["file_read".into(), "web_search".into()]),
        )
        .unwrap();

        assert_eq!(
            job.allowed_tools,
            Some(vec!["file_read".into(), "web_search".into()])
        );

        let stored = get_job(&config, &job.id).unwrap();
        assert_eq!(stored.allowed_tools, job.allowed_tools);
    }

    #[test]
    fn update_job_persists_allowed_tools_patch() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_agent_job(
            &config,
            Some("agent".into()),
            Schedule::Every { every_ms: 60_000 },
            "do work",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
        )
        .unwrap();

        let updated = update_job(
            &config,
            &job.id,
            CronJobPatch {
                allowed_tools: Some(vec!["shell".into()]),
                ..CronJobPatch::default()
            },
        )
        .unwrap();

        assert_eq!(updated.allowed_tools, Some(vec!["shell".into()]));
        assert_eq!(
            get_job(&config, &job.id).unwrap().allowed_tools,
            Some(vec!["shell".into()])
        );
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
        assert_eq!(stored.last_output.as_deref(), Some("failed output"));
    }

    #[test]
    fn job_type_from_sql_reads_valid_value() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let now = Utc::now();

        with_connection(&config, |conn| {
            conn.execute(
                "INSERT INTO cron_jobs (id, expression, command, schedule, job_type, created_at, next_run)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    "job-type-valid",
                    "*/5 * * * *",
                    "echo ok",
                    Option::<String>::None,
                    "agent",
                    now.to_rfc3339(),
                    (now + ChronoDuration::minutes(5)).to_rfc3339(),
                ],
            )?;
            Ok(())
        })
        .unwrap();

        let job = get_job(&config, "job-type-valid").unwrap();
        assert_eq!(job.job_type, JobType::Agent);
    }

    #[test]
    fn job_type_from_sql_rejects_invalid_value() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let now = Utc::now();

        with_connection(&config, |conn| {
            conn.execute(
                "INSERT INTO cron_jobs (id, expression, command, schedule, job_type, created_at, next_run)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    "job-type-invalid",
                    "*/5 * * * *",
                    "echo ok",
                    Option::<String>::None,
                    "unknown",
                    now.to_rfc3339(),
                    (now + ChronoDuration::minutes(5)).to_rfc3339(),
                ],
            )?;
            Ok(())
        })
        .unwrap();

        assert!(get_job(&config, "job-type-invalid").is_err());
    }

    #[test]
    fn migration_falls_back_to_legacy_expression() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        with_connection(&config, |conn| {
            conn.execute(
                "INSERT INTO cron_jobs (id, expression, command, created_at, next_run)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "legacy-id",
                    "*/5 * * * *",
                    "echo legacy",
                    Utc::now().to_rfc3339(),
                    (Utc::now() + ChronoDuration::minutes(5)).to_rfc3339(),
                ],
            )?;
            conn.execute(
                "UPDATE cron_jobs SET schedule = NULL WHERE id = 'legacy-id'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let job = get_job(&config, "legacy-id").unwrap();
        assert!(matches!(job.schedule, Schedule::Cron { .. }));
    }

    #[test]
    fn record_and_prune_runs() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.cron.max_run_history = 2;
        let job = add_job(&config, "*/5 * * * *", "echo ok").unwrap();
        let base = Utc::now();

        for idx in 0..3 {
            let start = base + ChronoDuration::seconds(idx);
            let end = start + ChronoDuration::milliseconds(100);
            record_run(&config, &job.id, start, end, "ok", Some("done"), 100).unwrap();
        }

        let runs = list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn remove_job_cascades_run_history() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_job(&config, "*/5 * * * *", "echo ok").unwrap();
        let start = Utc::now();
        record_run(
            &config,
            &job.id,
            start,
            start + ChronoDuration::milliseconds(5),
            "ok",
            Some("ok"),
            5,
        )
        .unwrap();

        remove_job(&config, &job.id).unwrap();
        let runs = list_runs(&config, &job.id, 10).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn record_run_truncates_large_output() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_job(&config, "*/5 * * * *", "echo trunc").unwrap();
        let output = "x".repeat(MAX_CRON_OUTPUT_BYTES + 512);

        record_run(
            &config,
            &job.id,
            Utc::now(),
            Utc::now(),
            "ok",
            Some(&output),
            1,
        )
        .unwrap();

        let runs = list_runs(&config, &job.id, 1).unwrap();
        let stored = runs[0].output.as_deref().unwrap_or_default();
        assert!(stored.ends_with(TRUNCATED_OUTPUT_MARKER));
        assert!(stored.len() <= MAX_CRON_OUTPUT_BYTES);
    }

    #[test]
    fn reschedule_after_run_disables_at_schedule_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = add_shell_job(&config, None, Schedule::At { at }, "echo once", None).unwrap();

        reschedule_after_run(&config, &job, true, "done").unwrap();

        let stored = get_job(&config, &job.id).unwrap();
        assert!(
            !stored.enabled,
            "At schedule job should be disabled after reschedule"
        );
        assert_eq!(stored.last_status.as_deref(), Some("ok"));
    }

    #[test]
    fn reschedule_after_run_disables_at_schedule_job_on_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = add_shell_job(&config, None, Schedule::At { at }, "echo once", None).unwrap();

        reschedule_after_run(&config, &job, false, "failed").unwrap();

        let stored = get_job(&config, &job.id).unwrap();
        assert!(
            !stored.enabled,
            "At schedule job should be disabled after reschedule even on failure"
        );
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert_eq!(stored.last_output.as_deref(), Some("failed"));
    }

    #[test]
    fn reschedule_after_run_truncates_last_output() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_job(&config, "*/5 * * * *", "echo trunc").unwrap();
        let output = "y".repeat(MAX_CRON_OUTPUT_BYTES + 1024);

        reschedule_after_run(&config, &job, false, &output).unwrap();

        let stored = get_job(&config, &job.id).unwrap();
        let last_output = stored.last_output.as_deref().unwrap_or_default();
        assert!(last_output.ends_with(TRUNCATED_OUTPUT_MARKER));
        assert!(last_output.len() <= MAX_CRON_OUTPUT_BYTES);
    }

    // ── Declarative cron job sync tests ──────────────────────────

    fn make_shell_decl(id: &str, expr: &str, cmd: &str) -> zeroclaw_config::schema::CronJobDecl {
        zeroclaw_config::schema::CronJobDecl {
            id: id.to_string(),
            name: Some(format!("decl-{id}")),
            job_type: "shell".to_string(),
            schedule: zeroclaw_config::schema::CronScheduleDecl::Cron {
                expr: expr.to_string(),
                tz: None,
            },
            command: Some(cmd.to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            uses_memory: true,
            session_target: None,
            delivery: None,
        }
    }

    fn make_agent_decl(id: &str, expr: &str, prompt: &str) -> zeroclaw_config::schema::CronJobDecl {
        zeroclaw_config::schema::CronJobDecl {
            id: id.to_string(),
            name: Some(format!("decl-{id}")),
            job_type: "agent".to_string(),
            schedule: zeroclaw_config::schema::CronScheduleDecl::Cron {
                expr: expr.to_string(),
                tz: None,
            },
            command: None,
            prompt: Some(prompt.to_string()),
            enabled: true,
            model: None,
            allowed_tools: None,
            uses_memory: true,
            session_target: None,
            delivery: None,
        }
    }

    #[test]
    fn sync_inserts_new_declarative_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let decls = vec![make_shell_decl("daily-backup", "0 2 * * *", "echo backup")];
        sync_declarative_jobs(&config, &decls).unwrap();

        let job = get_job(&config, "daily-backup").unwrap();
        assert_eq!(job.command, "echo backup");
        assert_eq!(job.source, "declarative");
        assert_eq!(job.name.as_deref(), Some("decl-daily-backup"));
    }

    #[test]
    fn sync_updates_existing_declarative_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let decls = vec![make_shell_decl("updatable", "0 2 * * *", "echo v1")];
        sync_declarative_jobs(&config, &decls).unwrap();

        let job_v1 = get_job(&config, "updatable").unwrap();
        assert_eq!(job_v1.command, "echo v1");

        let decls_v2 = vec![make_shell_decl("updatable", "0 3 * * *", "echo v2")];
        sync_declarative_jobs(&config, &decls_v2).unwrap();

        let job_v2 = get_job(&config, "updatable").unwrap();
        assert_eq!(job_v2.command, "echo v2");
        assert_eq!(job_v2.expression, "0 3 * * *");
        assert_eq!(job_v2.source, "declarative");
    }

    #[test]
    fn sync_does_not_delete_imperative_jobs() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        // Create an imperative job via the normal API.
        let imperative = add_job(&config, "*/10 * * * *", "echo imperative").unwrap();

        // Sync declarative jobs (none of which match the imperative job).
        let decls = vec![make_shell_decl("my-decl", "0 2 * * *", "echo decl")];
        sync_declarative_jobs(&config, &decls).unwrap();

        // Imperative job should still exist.
        let still_there = get_job(&config, &imperative.id).unwrap();
        assert_eq!(still_there.command, "echo imperative");
        assert_eq!(still_there.source, "imperative");

        // Declarative job should also exist.
        let decl_job = get_job(&config, "my-decl").unwrap();
        assert_eq!(decl_job.command, "echo decl");
    }

    #[test]
    fn sync_removes_stale_declarative_jobs() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        // Insert two declarative jobs.
        let decls = vec![
            make_shell_decl("keeper", "0 2 * * *", "echo keep"),
            make_shell_decl("stale", "0 3 * * *", "echo stale"),
        ];
        sync_declarative_jobs(&config, &decls).unwrap();

        // Now sync with only "keeper" — "stale" should be removed.
        let decls_v2 = vec![make_shell_decl("keeper", "0 2 * * *", "echo keep")];
        sync_declarative_jobs(&config, &decls_v2).unwrap();

        assert!(get_job(&config, "stale").is_err());
        assert!(get_job(&config, "keeper").is_ok());
    }

    #[test]
    fn sync_empty_removes_all_declarative_jobs() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let decls = vec![make_shell_decl("to-remove", "0 2 * * *", "echo bye")];
        sync_declarative_jobs(&config, &decls).unwrap();
        assert!(get_job(&config, "to-remove").is_ok());

        // Sync with empty list.
        sync_declarative_jobs(&config, &[]).unwrap();
        assert!(get_job(&config, "to-remove").is_err());
    }

    #[test]
    fn sync_validates_shell_job_requires_command() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let mut decl = make_shell_decl("bad", "0 2 * * *", "echo ok");
        decl.command = None;

        let result = sync_declarative_jobs(&config, &[decl]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[test]
    fn sync_validates_agent_job_requires_prompt() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let mut decl = make_agent_decl("bad-agent", "0 2 * * *", "do stuff");
        decl.prompt = None;

        let result = sync_declarative_jobs(&config, &[decl]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prompt"));
    }

    #[test]
    fn sync_agent_job_inserts_correctly() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let decls = vec![make_agent_decl(
            "agent-check",
            "*/15 * * * *",
            "check health",
        )];
        sync_declarative_jobs(&config, &decls).unwrap();

        let job = get_job(&config, "agent-check").unwrap();
        assert_eq!(job.job_type, JobType::Agent);
        assert_eq!(job.prompt.as_deref(), Some("check health"));
        assert_eq!(job.source, "declarative");
    }

    #[test]
    fn sync_every_schedule_works() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let decl = zeroclaw_config::schema::CronJobDecl {
            id: "interval-job".to_string(),
            name: None,
            job_type: "shell".to_string(),
            schedule: zeroclaw_config::schema::CronScheduleDecl::Every { every_ms: 60000 },
            command: Some("echo interval".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            uses_memory: true,
            session_target: None,
            delivery: None,
        };

        sync_declarative_jobs(&config, &[decl]).unwrap();

        let job = get_job(&config, "interval-job").unwrap();
        assert!(matches!(job.schedule, Schedule::Every { every_ms: 60000 }));
        assert_eq!(job.command, "echo interval");
    }

    #[test]
    fn declarative_config_parses_from_toml() {
        let toml_str = r#"
enabled = true

[[jobs]]
id = "daily-report"
name = "Daily Report"
job_type = "shell"
command = "echo report"
schedule = { kind = "cron", expr = "0 9 * * *" }

[[jobs]]
id = "health-check"
job_type = "agent"
prompt = "Check server health"
schedule = { kind = "every", every_ms = 300000 }
        "#;

        let parsed: zeroclaw_config::schema::CronConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.jobs.len(), 2);

        assert_eq!(parsed.jobs[0].id, "daily-report");
        assert_eq!(parsed.jobs[0].command.as_deref(), Some("echo report"));
        assert!(matches!(
            parsed.jobs[0].schedule,
            zeroclaw_config::schema::CronScheduleDecl::Cron { ref expr, .. } if expr == "0 9 * * *"
        ));

        assert_eq!(parsed.jobs[1].id, "health-check");
        assert_eq!(parsed.jobs[1].job_type, "agent");
        assert_eq!(
            parsed.jobs[1].prompt.as_deref(),
            Some("Check server health")
        );
        assert!(matches!(
            parsed.jobs[1].schedule,
            zeroclaw_config::schema::CronScheduleDecl::Every { every_ms: 300_000 }
        ));
    }
}
