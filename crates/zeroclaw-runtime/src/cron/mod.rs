use crate::security::SecurityPolicy;
use anyhow::{Result, anyhow, bail};
use zeroclaw_config::schema::Config;

mod schedule;
mod store;
mod types;

pub mod scheduler;

#[allow(unused_imports)]
pub use schedule::{
    next_run_for_schedule, normalize_expression, schedule_cron_expression, validate_schedule,
};
#[allow(unused_imports)]
pub use store::{
    add_agent_job, add_announce_job, all_overdue_jobs, due_jobs, get_job, list_jobs, list_runs,
    record_last_run, record_run, remove_job, reschedule_after_run, sync_declarative_jobs,
    update_job,
};
pub use types::{
    CronJob, CronJobPatch, CronRun, DeliveryConfig, JobType, Schedule, SessionTarget,
    deserialize_maybe_stringified,
};

/// Validate a shell command against the full security policy (allowlist + risk gate).
///
/// Returns `Ok(())` if the command passes all checks, or an error describing
/// why it was blocked.
pub fn validate_shell_command(config: &Config, command: &str, approved: bool) -> Result<()> {
    let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
    validate_shell_command_with_security(&security, command, approved)
}

/// Validate a shell command using an existing `SecurityPolicy` instance.
///
/// Preferred when the caller already holds a `SecurityPolicy` (e.g. scheduler).
pub fn validate_shell_command_with_security(
    security: &SecurityPolicy,
    command: &str,
    approved: bool,
) -> Result<()> {
    security
        .validate_command_execution(command, approved)
        .map(|_| ())
        .map_err(|reason| anyhow!("blocked by security policy: {reason}"))
}

pub fn validate_delivery_config(delivery: Option<&DeliveryConfig>) -> Result<()> {
    let Some(delivery) = delivery else {
        return Ok(());
    };

    if delivery.mode.eq_ignore_ascii_case("none") {
        return Ok(());
    }
    if !delivery.mode.eq_ignore_ascii_case("announce") {
        bail!("unsupported delivery mode: {}", delivery.mode);
    }

    let channel = delivery.channel.as_deref().map(str::trim);
    let Some(channel) = channel.filter(|value| !value.is_empty()) else {
        bail!("delivery.channel is required when configuring channel delivery");
    };
    match channel.to_ascii_lowercase().as_str() {
        "telegram" | "discord" | "slack" | "mattermost" | "signal" | "matrix" | "qq" => {}
        other => bail!("unsupported delivery channel: {other}"),
    }

    let has_target = delivery
        .to
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if !has_target {
        bail!("delivery.to is required when delivery.channel is set");
    }

    Ok(())
}

/// Create a validated shell job, enforcing security policy before persistence.
///
/// All entrypoints that create shell cron jobs should route through this
/// function to guarantee consistent policy enforcement.
pub fn add_shell_job_with_approval(
    config: &Config,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
) -> Result<CronJob> {
    validate_shell_command(config, command, approved)?;
    validate_delivery_config(delivery.as_ref())?;
    store::add_shell_job(config, name, schedule, command, delivery)
}

/// Update a shell job's command with security validation.
///
/// Validates the new command (if changed) before persisting.
pub fn update_shell_job_with_approval(
    config: &Config,
    job_id: &str,
    patch: CronJobPatch,
    approved: bool,
) -> Result<CronJob> {
    if let Some(command) = patch.command.as_deref() {
        validate_shell_command(config, command, approved)?;
    }
    update_job(config, job_id, patch)
}

/// Create a one-shot validated shell job from a delay string (e.g. "30m").
pub fn add_once_validated(
    config: &Config,
    delay: &str,
    command: &str,
    approved: bool,
) -> Result<CronJob> {
    let duration = parse_delay(delay)?;
    let at = chrono::Utc::now() + duration;
    add_once_at_validated(config, at, command, approved)
}

/// Create a one-shot validated shell job at an absolute timestamp.
pub fn add_once_at_validated(
    config: &Config,
    at: chrono::DateTime<chrono::Utc>,
    command: &str,
    approved: bool,
) -> Result<CronJob> {
    let schedule = Schedule::At { at };
    add_shell_job_with_approval(config, None, schedule, command, None, approved)
}

// Convenience wrappers for CLI paths (default approved=false).

pub fn add_shell_job(
    config: &Config,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
) -> Result<CronJob> {
    add_shell_job_with_approval(config, name, schedule, command, None, false)
}

pub fn add_job(config: &Config, expression: &str, command: &str) -> Result<CronJob> {
    let schedule = Schedule::Cron {
        expr: expression.to_string(),
        tz: None,
    };
    add_shell_job(config, None, schedule, command)
}

#[allow(clippy::needless_pass_by_value)]
pub fn add_once(config: &Config, delay: &str, command: &str) -> Result<CronJob> {
    add_once_validated(config, delay, command, false)
}

pub fn add_once_at(
    config: &Config,
    at: chrono::DateTime<chrono::Utc>,
    command: &str,
) -> Result<CronJob> {
    add_once_at_validated(config, at, command, false)
}

pub fn pause_job(config: &Config, id: &str) -> Result<CronJob> {
    update_job(
        config,
        id,
        CronJobPatch {
            enabled: Some(false),
            ..CronJobPatch::default()
        },
    )
}

pub fn resume_job(config: &Config, id: &str) -> Result<CronJob> {
    update_job(
        config,
        id,
        CronJobPatch {
            enabled: Some(true),
            ..CronJobPatch::default()
        },
    )
}

pub fn parse_delay(input: &str) -> Result<chrono::Duration> {
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!("delay must not be empty");
    }
    let split = input
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(input.len());
    let (num, unit) = input.split_at(split);
    let amount: i64 = num.parse()?;
    let unit = if unit.is_empty() { "m" } else { unit };
    let duration = match unit {
        "s" => chrono::Duration::seconds(amount),
        "m" => chrono::Duration::minutes(amount),
        "h" => chrono::Duration::hours(amount),
        "d" => chrono::Duration::days(amount),
        _ => anyhow::bail!("unsupported delay unit '{unit}', use s/m/h/d"),
    };
    Ok(duration)
}
