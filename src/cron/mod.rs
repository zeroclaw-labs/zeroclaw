pub use zeroclaw_runtime::cron::*;

use crate::config::Config;
use anyhow::{Result, bail};

fn build_delivery(
    channel: Option<String>,
    to: Option<String>,
) -> Result<Option<DeliveryConfig>> {
    match (channel, to) {
        (None, None) => Ok(None),
        (Some(c), Some(t)) => Ok(Some(DeliveryConfig {
            mode: "announce".to_string(),
            channel: Some(c),
            to: Some(t),
            best_effort: true,
        })),
        (Some(_), None) => bail!("--delivery-to is required when --delivery-channel is set"),
        (None, Some(_)) => bail!("--delivery-channel is required when --delivery-to is set"),
    }
}

fn print_delivery_summary(delivery: &DeliveryConfig) {
    if delivery.mode.eq_ignore_ascii_case("announce") {
        println!(
            "  Deliver: {} → {}",
            delivery.channel.as_deref().unwrap_or("?"),
            delivery.to.as_deref().unwrap_or("?"),
        );
    }
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
                if job.delivery.mode.eq_ignore_ascii_case("announce") {
                    println!(
                        "    deliver: {} → {}",
                        job.delivery.channel.as_deref().unwrap_or("?"),
                        job.delivery.to.as_deref().unwrap_or("?"),
                    );
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
            command,
        } => {
            let schedule = Schedule::Cron {
                expr: expression,
                tz,
            };
            let delivery = build_delivery(delivery_channel, delivery_to)?;
            if agent {
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
                print_delivery_summary(&job.delivery);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job = add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added cron job {}", job.id);
                println!("  Expr: {}", job.expression);
                println!("  Next: {}", job.next_run.to_rfc3339());
                println!("  Cmd : {}", job.command);
                print_delivery_summary(&job.delivery);
            }
            Ok(())
        }
        crate::CronCommands::AddAt {
            at,
            agent,
            allowed_tools,
            delivery_channel,
            delivery_to,
            command,
        } => {
            let at = chrono::DateTime::parse_from_rfc3339(&at)
                .map_err(|e| anyhow::anyhow!("Invalid RFC3339 timestamp for --at: {e}"))?
                .with_timezone(&chrono::Utc);
            let schedule = Schedule::At { at };
            let delivery = build_delivery(delivery_channel, delivery_to)?;
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
                print_delivery_summary(&job.delivery);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job = add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added one-shot cron job {}", job.id);
                println!("  At  : {}", job.next_run.to_rfc3339());
                println!("  Cmd : {}", job.command);
                print_delivery_summary(&job.delivery);
            }
            Ok(())
        }
        crate::CronCommands::AddEvery {
            every_ms,
            agent,
            allowed_tools,
            delivery_channel,
            delivery_to,
            command,
        } => {
            let schedule = Schedule::Every { every_ms };
            let delivery = build_delivery(delivery_channel, delivery_to)?;
            if agent {
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
                print_delivery_summary(&job.delivery);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job = add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added interval cron job {}", job.id);
                println!("  Every(ms): {every_ms}");
                println!("  Next     : {}", job.next_run.to_rfc3339());
                println!("  Cmd      : {}", job.command);
                print_delivery_summary(&job.delivery);
            }
            Ok(())
        }
        crate::CronCommands::Once {
            delay,
            agent,
            allowed_tools,
            delivery_channel,
            delivery_to,
            command,
        } => {
            let delivery = build_delivery(delivery_channel, delivery_to)?;
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
                print_delivery_summary(&job.delivery);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --agent cron jobs");
                }
                let job = add_shell_job_with_approval(config, None, schedule, &command, delivery, false)?;
                println!("✅ Added one-shot cron job {}", job.id);
                println!("  At  : {}", job.next_run.to_rfc3339());
                println!("  Cmd : {}", job.command);
                print_delivery_summary(&job.delivery);
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
            delivery_none,
        } => {
            let delivery_patch = if delivery_none {
                Some(DeliveryConfig::default())
            } else {
                build_delivery(delivery_channel, delivery_to)?
            };

            if expression.is_none()
                && tz.is_none()
                && command.is_none()
                && name.is_none()
                && allowed_tools.is_empty()
                && delivery_patch.is_none()
            {
                bail!(
                    "At least one of --expression, --tz, --command, --name, --allowed-tool, --delivery-channel, or --delivery-none must be provided"
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

            if let Some(d) = delivery_patch.as_ref() {
                validate_delivery_config(Some(d))?;
            }

            let patch = CronJobPatch {
                schedule,
                command,
                name,
                delivery: delivery_patch,
                allowed_tools: if allowed_tools.is_empty() {
                    None
                } else {
                    Some(allowed_tools)
                },
                ..CronJobPatch::default()
            };

            let job = update_shell_job_with_approval(config, &id, patch, false)?;
            println!("\u{2705} Updated cron job {}", job.id);
            println!("  Expr: {}", job.expression);
            println!("  Next: {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            print_delivery_summary(&job.delivery);
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
