use crate::config::Config;
use anyhow::Result;

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
    add_agent_job, add_job, add_shell_job, due_jobs, get_job, list_jobs, list_runs, record_last_run,
    record_run, remove_job, reschedule_after_run, update_job,
};
pub use types::{
    CronJob, CronJobPatch, CronRun, DeliveryConfig, JobType, Schedule, SessionTarget,
};

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
                    "- {} | {:?} | next={} | last={} ({})",
                    job.id,
                    job.schedule,
                    job.next_run.to_rfc3339(),
                    last_run,
                    last_status,
                );
                if !job.command.is_empty() {
                    println!("    cmd: {}", job.command);
                }
                if let Some(prompt) = &job.prompt {
                    println!("    prompt: {prompt}");
                }
            }
            Ok(())
        }
        crate::CronCommands::Add {
            expression,
            tz,
            command,
        } => {
            let schedule = Schedule::Cron {
                expr: expression,
                tz,
            };
            let job = add_shell_job(config, None, schedule, &command)?;
            println!("✅ Added cron job {}", job.id);
            println!("  Expr: {}", job.expression);
            println!("  Next: {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            Ok(())
        }
        crate::CronCommands::AddAt { at, command } => {
            let at = chrono::DateTime::parse_from_rfc3339(&at)
                .map_err(|e| anyhow::anyhow!("Invalid RFC3339 timestamp for --at: {e}"))?
                .with_timezone(&chrono::Utc);
            let schedule = Schedule::At { at };
            let job = add_shell_job(config, None, schedule, &command)?;
            println!("✅ Added one-shot cron job {}", job.id);
            println!("  At  : {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            Ok(())
        }
        crate::CronCommands::AddEvery { every_ms, command } => {
            let schedule = Schedule::Every { every_ms };
            let job = add_shell_job(config, None, schedule, &command)?;
            println!("✅ Added interval cron job {}", job.id);
            println!("  Every(ms): {every_ms}");
            println!("  Next     : {}", job.next_run.to_rfc3339());
            println!("  Cmd      : {}", job.command);
            Ok(())
        }
        crate::CronCommands::Once { delay, command } => {
            let job = add_once(config, &delay, &command)?;
            println!("✅ Added one-shot cron job {}", job.id);
            println!("  At  : {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            Ok(())
        }
        crate::CronCommands::Remove { id } => remove_job(config, &id),
        crate::CronCommands::Pause { id } => {
            pause_job(config, &id)?;
            println!("⏸️  Paused cron job {id}");
            Ok(())
        }
        crate::CronCommands::Resume { id } => {
            resume_job(config, &id)?;
            println!("▶️  Resumed cron job {id}");
            Ok(())
        }
    }
}

pub fn add_once(config: &Config, delay: &str, command: &str) -> Result<CronJob> {
    let duration = parse_delay(delay)?;
    let at = chrono::Utc::now() + duration;
    add_once_at(config, at, command)
}

pub fn add_once_at(
    config: &Config,
    at: chrono::DateTime<chrono::Utc>,
    command: &str,
) -> Result<CronJob> {
    let schedule = Schedule::At { at };
    add_shell_job(config, None, schedule, command)
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

fn parse_delay(input: &str) -> Result<chrono::Duration> {
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
