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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use anyhow::Result;
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

    fn make_job(config: &Config, expr: &str, tz: Option<&str>, cmd: &str) -> CronJob {
        add_shell_job(
            config,
            None,
            Schedule::Cron {
                expr: expr.into(),
                tz: tz.map(Into::into),
            },
            cmd,
        )
        .unwrap()
    }

    fn run_update(
        config: &Config,
        id: &str,
        expression: Option<&str>,
        tz: Option<&str>,
        command: Option<&str>,
        name: Option<&str>,
    ) -> Result<()> {
        handle_command(
            crate::CronCommands::Update {
                id: id.into(),
                expression: expression.map(Into::into),
                tz: tz.map(Into::into),
                command: command.map(Into::into),
                name: name.map(Into::into),
                allowed_tools: vec![],
                delivery_channel: None,
                delivery_to: None,
                no_best_effort: false,
            },
            config,
        )
    }

    #[test]
    fn update_changes_command_via_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo original");

        run_update(&config, &job.id, None, None, Some("echo updated"), None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.command, "echo updated");
        assert_eq!(updated.id, job.id);
    }

    #[test]
    fn update_changes_expression_via_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        run_update(&config, &job.id, Some("0 9 * * *"), None, None, None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.expression, "0 9 * * *");
    }

    #[test]
    fn update_changes_name_via_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        run_update(&config, &job.id, None, None, None, Some("new-name")).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.name.as_deref(), Some("new-name"));
    }

    #[test]
    fn update_tz_alone_sets_timezone() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        run_update(
            &config,
            &job.id,
            None,
            Some("America/Los_Angeles"),
            None,
            None,
        )
        .unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(
            updated.schedule,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: Some("America/Los_Angeles".into()),
            }
        );
    }

    #[test]
    fn update_expression_preserves_existing_tz() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(
            &config,
            "*/5 * * * *",
            Some("America/Los_Angeles"),
            "echo test",
        );

        run_update(&config, &job.id, Some("0 9 * * *"), None, None, None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(
            updated.schedule,
            Schedule::Cron {
                expr: "0 9 * * *".into(),
                tz: Some("America/Los_Angeles".into()),
            }
        );
    }

    #[test]
    fn update_preserves_unchanged_fields() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_shell_job(
            &config,
            Some("original-name".into()),
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo original",
        )
        .unwrap();

        run_update(&config, &job.id, None, None, Some("echo changed"), None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.command, "echo changed");
        assert_eq!(updated.name.as_deref(), Some("original-name"));
        assert_eq!(updated.expression, "*/5 * * * *");
    }

    #[test]
    fn update_no_flags_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        let result = run_update(&config, &job.id, None, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("At least one of"));
    }

    #[test]
    fn update_nonexistent_job_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = run_update(
            &config,
            "nonexistent-id",
            None,
            None,
            Some("echo test"),
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn update_security_allows_safe_command() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
        assert!(security.is_command_allowed("echo safe"));
    }

    #[test]
    fn add_shell_job_requires_explicit_approval_for_medium_risk() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into(), "touch".into()];

        let denied = add_shell_job(
            &config,
            None,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "touch cron-medium-risk",
        );
        assert!(denied.is_err());
        assert!(
            denied
                .unwrap_err()
                .to_string()
                .contains("explicit approval")
        );

        let approved = add_shell_job_with_approval(
            &config,
            None,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "touch cron-medium-risk",
            None,
            true,
        );
        assert!(approved.is_ok(), "{approved:?}");
    }

    #[test]
    fn update_requires_explicit_approval_for_medium_risk() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into(), "touch".into()];
        let job = make_job(&config, "*/5 * * * *", None, "echo original");

        let denied = update_shell_job_with_approval(
            &config,
            &job.id,
            CronJobPatch {
                command: Some("touch cron-medium-risk-update".into()),
                ..CronJobPatch::default()
            },
            false,
        );
        assert!(denied.is_err());
        assert!(
            denied
                .unwrap_err()
                .to_string()
                .contains("explicit approval")
        );

        let approved = update_shell_job_with_approval(
            &config,
            &job.id,
            CronJobPatch {
                command: Some("touch cron-medium-risk-update".into()),
                ..CronJobPatch::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(approved.command, "touch cron-medium-risk-update");
    }

    #[test]
    fn cli_update_requires_explicit_approval_for_medium_risk() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into(), "touch".into()];
        let job = make_job(&config, "*/5 * * * *", None, "echo original");

        let result = run_update(
            &config,
            &job.id,
            None,
            None,
            Some("touch cron-cli-medium-risk"),
            None,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("explicit approval")
        );
    }

    #[test]
    fn add_once_validated_creates_one_shot_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = add_once_validated(&config, "1h", "echo one-shot", false).unwrap();
        assert_eq!(job.command, "echo one-shot");
        assert!(matches!(job.schedule, Schedule::At { .. }));
    }

    #[test]
    fn add_once_validated_blocks_disallowed_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into()];
        config.autonomy.level = crate::security::AutonomyLevel::Supervised;

        let result = add_once_validated(&config, "1h", "curl https://example.com", false);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("blocked by security policy")
        );
    }

    #[test]
    fn add_once_at_validated_creates_one_shot_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let at = chrono::Utc::now() + chrono::Duration::hours(1);

        let job = add_once_at_validated(&config, at, "echo at-shot", false).unwrap();
        assert_eq!(job.command, "echo at-shot");
        assert!(matches!(job.schedule, Schedule::At { .. }));
    }

    #[test]
    fn add_once_at_validated_blocks_medium_risk_without_approval() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into(), "touch".into()];
        let at = chrono::Utc::now() + chrono::Duration::hours(1);

        let denied = add_once_at_validated(&config, at, "touch at-medium", false);
        assert!(denied.is_err());
        assert!(
            denied
                .unwrap_err()
                .to_string()
                .contains("explicit approval")
        );

        let approved = add_once_at_validated(&config, at, "touch at-medium", true);
        assert!(approved.is_ok(), "{approved:?}");
    }

    #[test]
    fn gateway_api_path_validates_shell_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into()];
        config.autonomy.level = crate::security::AutonomyLevel::Supervised;

        // Simulate gateway API path: add_shell_job_with_approval(approved=false)
        let result = add_shell_job_with_approval(
            &config,
            None,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "curl https://example.com",
            None,
            false,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("blocked by security policy")
        );
    }

    #[test]
    fn scheduler_path_validates_shell_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into()];
        config.autonomy.level = crate::security::AutonomyLevel::Supervised;

        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
        // Simulate scheduler validation path
        let result =
            validate_shell_command_with_security(&security, "curl https://example.com", false);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("blocked by security policy")
        );
    }

    #[test]
    fn cli_agent_flag_creates_agent_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/15 * * * *".into(),
                tz: None,
                agent: true,
                allowed_tools: vec![],
                delivery_channel: None,
                delivery_to: None,
                no_best_effort: false,
                count: None,
                until: None,
                command: "Check server health: disk space, memory, CPU load".into(),
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, JobType::Agent);
        assert_eq!(
            jobs[0].prompt.as_deref(),
            Some("Check server health: disk space, memory, CPU load")
        );
    }

    #[test]
    fn cli_agent_flag_bypasses_shell_security_validation() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into()];
        config.autonomy.level = crate::security::AutonomyLevel::Supervised;

        // Without --agent, a natural language string would be blocked by shell
        // security policy. With --agent, it routes to agent job and skips
        // shell validation entirely.
        let result = handle_command(
            crate::CronCommands::Add {
                expression: "*/15 * * * *".into(),
                tz: None,
                agent: true,
                allowed_tools: vec![],
                delivery_channel: None,
                delivery_to: None,
                no_best_effort: false,
                count: None,
                until: None,
                command: "Check server health: disk space, memory, CPU load".into(),
            },
            &config,
        );
        assert!(result.is_ok());

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, JobType::Agent);
    }

    #[test]
    fn cli_agent_allowed_tools_persist() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/15 * * * *".into(),
                tz: None,
                agent: true,
                allowed_tools: vec!["file_read".into(), "web_search".into()],
                delivery_channel: None,
                delivery_to: None,
                no_best_effort: false,
                count: None,
                until: None,
                command: "Check server health".into(),
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].allowed_tools,
            Some(vec!["file_read".into(), "web_search".into()])
        );
    }

    #[test]
    fn cli_update_agent_allowed_tools_persist() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_agent_job(
            &config,
            Some("agent".into()),
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "original prompt",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
        )
        .unwrap();

        handle_command(
            crate::CronCommands::Update {
                id: job.id.clone(),
                expression: None,
                tz: None,
                command: None,
                name: None,
                allowed_tools: vec!["shell".into()],
                delivery_channel: None,
                delivery_to: None,
                no_best_effort: false,
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.allowed_tools, Some(vec!["shell".into()]));
    }

    #[test]
    fn cli_add_persists_delivery_config() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                tz: None,
                agent: false,
                allowed_tools: vec![],
                delivery_channel: Some("telegram".into()),
                delivery_to: Some("12345".into()),
                no_best_effort: true,
                count: None,
                until: None,
                command: "echo ok".into(),
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].delivery.mode, "announce");
        assert_eq!(jobs[0].delivery.channel.as_deref(), Some("telegram"));
        assert_eq!(jobs[0].delivery.to.as_deref(), Some("12345"));
        assert!(!jobs[0].delivery.best_effort);
    }

    #[test]
    fn cli_update_patches_delivery_config() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        handle_command(
            crate::CronCommands::Update {
                id: job.id.clone(),
                expression: None,
                tz: None,
                command: None,
                name: None,
                allowed_tools: vec![],
                delivery_channel: Some("discord".into()),
                delivery_to: Some("chan-42".into()),
                no_best_effort: false,
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.delivery.mode, "announce");
        assert_eq!(updated.delivery.channel.as_deref(), Some("discord"));
        assert_eq!(updated.delivery.to.as_deref(), Some("chan-42"));
        assert!(updated.delivery.best_effort);
    }

    #[test]
    fn cli_add_without_channel_uses_default_none_delivery() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                tz: None,
                agent: false,
                allowed_tools: vec![],
                delivery_channel: None,
                delivery_to: None,
                no_best_effort: false,
                count: None,
                until: None,
                command: "echo ok".into(),
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs[0].delivery.mode, "none");
    }

    #[test]
    fn cli_without_agent_flag_defaults_to_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                tz: None,
                agent: false,
                allowed_tools: vec![],
                delivery_channel: None,
                delivery_to: None,
                no_best_effort: false,
                count: None,
                until: None,
                command: "echo ok".into(),
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, JobType::Shell);
        assert_eq!(jobs[0].command, "echo ok");
    }
}
