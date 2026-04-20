pub use zeroclaw_runtime::cron::*;

use crate::config::Config;
use anyhow::{Result, bail};

fn build_delivery(
    channel: Option<String>,
    to: Option<String>,
    no_best_effort: bool,
) -> Option<DeliveryConfig> {
    if channel.is_none() && to.is_none() {
        return None;
    }
    Some(DeliveryConfig {
        mode: "announce".to_string(),
        channel,
        to,
        best_effort: !no_best_effort,
    })
}

fn parse_until(raw: Option<String>) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    match raw {
        None => Ok(None),
        Some(s) => {
            let ts = chrono::DateTime::parse_from_rfc3339(&s)
                .map_err(|e| anyhow::anyhow!("Invalid RFC3339 timestamp for --until: {e}"))?
                .with_timezone(&chrono::Utc);
            Ok(Some(ts))
        }
    }
}

fn apply_bounds_to_job(
    config: &Config,
    job_id: &str,
    count: Option<i64>,
    until: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    if count.is_none() && until.is_none() {
        return Ok(());
    }
    if let Some(n) = count
        && n <= 0
    {
        bail!("--count must be positive");
    }
    update_job(
        config,
        job_id,
        CronJobPatch {
            remaining_runs: count,
            expires_until: until,
            ..CronJobPatch::default()
        },
    )?;
    Ok(())
}

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
            agent,
            allowed_tools,
            delivery_channel,
            delivery_to,
            no_best_effort,
            count,
            until,
            command,
        } => {
            let schedule = Schedule::Cron {
                expr: expression,
                tz,
            };
            let delivery = build_delivery(delivery_channel, delivery_to, no_best_effort);
            let until = parse_until(until)?;
            let job_id = if agent {
                let job = add_agent_job(
                    config,
                    None,
                    schedule,
                    &command,
                    SessionTarget::Isolated,
                    None,
                    delivery,
                    false,
                    if allowed_tools.is_empty() {
                        None
                    } else {
                        Some(allowed_tools)
                    },
                )?;
                println!("✅ Added agent cron job {}", job.id);
                println!("  Expr  : {}", job.expression);
                println!("  Next  : {}", job.next_run.to_rfc3339());
                println!("  Prompt: {}", job.prompt.as_deref().unwrap_or_default());
                job.id
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job =
                    add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added cron job {}", job.id);
                println!("  Expr: {}", job.expression);
                println!("  Next: {}", job.next_run.to_rfc3339());
                println!("  Cmd : {}", job.command);
                job.id
            };
            apply_bounds_to_job(config, &job_id, count, until)?;
            Ok(())
        }
        crate::CronCommands::AddAt {
            at,
            agent,
            allowed_tools,
            delivery_channel,
            delivery_to,
            no_best_effort,
            command,
        } => {
            let at = chrono::DateTime::parse_from_rfc3339(&at)
                .map_err(|e| anyhow::anyhow!("Invalid RFC3339 timestamp for --at: {e}"))?
                .with_timezone(&chrono::Utc);
            let schedule = Schedule::At { at };
            let delivery = build_delivery(delivery_channel, delivery_to, no_best_effort);
            if agent {
                let job = add_agent_job(
                    config,
                    None,
                    schedule,
                    &command,
                    SessionTarget::Isolated,
                    None,
                    delivery,
                    true,
                    if allowed_tools.is_empty() {
                        None
                    } else {
                        Some(allowed_tools)
                    },
                )?;
                println!("✅ Added one-shot agent cron job {}", job.id);
                println!("  At    : {}", job.next_run.to_rfc3339());
                println!("  Prompt: {}", job.prompt.as_deref().unwrap_or_default());
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job =
                    add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added one-shot cron job {}", job.id);
                println!("  At  : {}", job.next_run.to_rfc3339());
                println!("  Cmd : {}", job.command);
            }
            Ok(())
        }
        crate::CronCommands::AddEvery {
            every_ms,
            agent,
            allowed_tools,
            delivery_channel,
            delivery_to,
            no_best_effort,
            count,
            until,
            command,
        } => {
            let schedule = Schedule::Every { every_ms };
            let delivery = build_delivery(delivery_channel, delivery_to, no_best_effort);
            let until = parse_until(until)?;
            let job_id = if agent {
                let job = add_agent_job(
                    config,
                    None,
                    schedule,
                    &command,
                    SessionTarget::Isolated,
                    None,
                    delivery,
                    false,
                    if allowed_tools.is_empty() {
                        None
                    } else {
                        Some(allowed_tools)
                    },
                )?;
                println!("✅ Added interval agent cron job {}", job.id);
                println!("  Every(ms): {every_ms}");
                println!("  Next     : {}", job.next_run.to_rfc3339());
                println!("  Prompt   : {}", job.prompt.as_deref().unwrap_or_default());
                job.id
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job =
                    add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added interval cron job {}", job.id);
                println!("  Every(ms): {every_ms}");
                println!("  Next     : {}", job.next_run.to_rfc3339());
                println!("  Cmd      : {}", job.command);
                job.id
            };
            apply_bounds_to_job(config, &job_id, count, until)?;
            Ok(())
        }
        crate::CronCommands::Once {
            delay,
            agent,
            allowed_tools,
            delivery_channel,
            delivery_to,
            no_best_effort,
            command,
        } => {
            let delivery = build_delivery(delivery_channel, delivery_to, no_best_effort);
            let duration = parse_delay(&delay)?;
            let at = chrono::Utc::now() + duration;
            let schedule = Schedule::At { at };
            if agent {
                let job = add_agent_job(
                    config,
                    None,
                    schedule,
                    &command,
                    SessionTarget::Isolated,
                    None,
                    delivery,
                    true,
                    if allowed_tools.is_empty() {
                        None
                    } else {
                        Some(allowed_tools)
                    },
                )?;
                println!("✅ Added one-shot agent cron job {}", job.id);
                println!("  At    : {}", job.next_run.to_rfc3339());
                println!("  Prompt: {}", job.prompt.as_deref().unwrap_or_default());
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job =
                    add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added one-shot cron job {}", job.id);
                println!("  At  : {}", job.next_run.to_rfc3339());
                println!("  Cmd : {}", job.command);
            }
            Ok(())
        }
        crate::CronCommands::Update {
            id,
            expression,
            tz,
            command,
            name,
            allowed_tools,
            delivery_channel,
            delivery_to,
            no_best_effort,
        } => {
            let delivery = build_delivery(delivery_channel, delivery_to, no_best_effort);
            if expression.is_none()
                && tz.is_none()
                && command.is_none()
                && name.is_none()
                && allowed_tools.is_empty()
                && delivery.is_none()
            {
                bail!(
                    "At least one of --expression, --tz, --command, --name, --allowed-tool, or --channel/--to must be provided"
                );
            }

            let existing = if expression.is_some() || tz.is_some() || !allowed_tools.is_empty() {
                Some(get_job(config, &id)?)
            } else {
                None
            };

            // Merge expression/tz with the existing schedule so that
            // --tz alone updates the timezone and --expression alone
            // preserves the existing timezone.
            let schedule = if expression.is_some() || tz.is_some() {
                let existing = existing
                    .as_ref()
                    .expect("existing job must be loaded when updating schedule");
                let (existing_expr, existing_tz) = match &existing.schedule {
                    Schedule::Cron {
                        expr,
                        tz: existing_tz,
                    } => (expr.clone(), existing_tz.clone()),
                    _ => bail!("Cannot update expression/tz on a non-cron schedule"),
                };
                Some(Schedule::Cron {
                    expr: expression.unwrap_or(existing_expr),
                    tz: tz.or(existing_tz),
                })
            } else {
                None
            };

            if !allowed_tools.is_empty() {
                let existing = existing
                    .as_ref()
                    .expect("existing job must be loaded when updating allowed tools");
                if existing.job_type != JobType::Agent {
                    bail!("--allowed-tool is only supported for agent cron jobs");
                }
            }

            let patch = CronJobPatch {
                schedule,
                command,
                name,
                allowed_tools: if allowed_tools.is_empty() {
                    None
                } else {
                    Some(allowed_tools)
                },
                delivery,
                ..CronJobPatch::default()
            };

            let job = update_shell_job_with_approval(config, &id, patch, false)?;
            println!("\u{2705} Updated cron job {}", job.id);
            println!("  Expr: {}", job.expression);
            println!("  Next: {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            Ok(())
        }
        crate::CronCommands::Announce {
            message,
            cron,
            tz,
            every,
            once,
            at,
            delivery_channel,
            delivery_to,
            no_best_effort,
            count,
            until,
        } => {
            let schedule = match (cron, every, once, at) {
                (Some(expr), None, None, None) => Schedule::Cron { expr, tz },
                (None, Some(ms), None, None) => Schedule::Every { every_ms: ms },
                (None, None, Some(delay), None) => {
                    let duration = parse_delay(&delay)?;
                    Schedule::At {
                        at: chrono::Utc::now() + duration,
                    }
                }
                (None, None, None, Some(ts)) => Schedule::At {
                    at: chrono::DateTime::parse_from_rfc3339(&ts)
                        .map_err(|e| anyhow::anyhow!("Invalid RFC3339 timestamp for --at: {e}"))?
                        .with_timezone(&chrono::Utc),
                },
                (None, None, None, None) => {
                    bail!("one of --cron, --every, --once, or --at is required")
                }
                _ => bail!("--cron, --every, --once, and --at are mutually exclusive"),
            };
            let delivery = DeliveryConfig {
                mode: "announce".to_string(),
                channel: Some(delivery_channel),
                to: Some(delivery_to),
                best_effort: !no_best_effort,
            };
            let until = parse_until(until)?;
            if let Some(n) = count
                && n <= 0
            {
                bail!("--count must be positive");
            }
            let job = add_announce_job(config, None, schedule, &message, delivery, count, until)?;
            println!("✅ Added announce job {}", job.id);
            println!("  Schedule: {:?}", job.schedule);
            println!("  Next    : {}", job.next_run.to_rfc3339());
            println!("  Channel : {:?}", job.delivery.channel);
            println!("  To      : {:?}", job.delivery.to);
            if let Some(n) = job.remaining_runs {
                println!("  Count   : {n}");
            }
            if let Some(u) = job.expires_until {
                println!("  Until   : {}", u.to_rfc3339());
            }
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
