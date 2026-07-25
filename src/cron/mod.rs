pub use zeroclaw_runtime::cron::*;

use crate::config::Config;
use anyhow::{Result, bail};
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

/// Bail with a clear error if the named agent isn't configured.
fn require_configured_agent(config: &Config, agent_alias: &str) -> Result<()> {
    if config.agent(agent_alias).is_none() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"agent_alias": agent_alias})),
            "cron CLI rejected: unknown agent alias"
        );
        anyhow::bail!("Unknown agent {agent_alias:?} (no [agents.{agent_alias}] entry configured)");
    }
    Ok(())
}

fn parse_explicit_rfc3339_utc(raw: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        .map_err(|err| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "raw": raw,
                        "error": format!("{}", err),
                    })),
                "cron --at rejected: timestamp lacks explicit Z/offset or is malformed"
            );
            anyhow::Error::msg(format!(
                "Invalid RFC3339 timestamp for --at: expected RFC3339 timestamp with explicit Z or offset, e.g. 2026-05-18T09:00:00Z or 2026-05-18T09:00:00-04:00; got '{raw}': {err}"
            ))
        })
}

/// Build a `DeliveryConfig` from the shared CLI delivery flags.
///
/// Returns `None` (leaving the job's delivery mode `"none"`) when no delivery
/// flag is set, so omitting the flags keeps the pre-existing behaviour. When a
/// flag is present the config is `"announce"`; `validate_delivery_config`,
/// called by the create/update paths, then enforces that channel and recipient
/// are both provided.
fn build_delivery(args: crate::CronDeliveryArgs) -> Option<DeliveryConfig> {
    if args.delivery_channel.is_none()
        && args.delivery_to.is_none()
        && args.delivery_thread.is_none()
    {
        return None;
    }
    Some(DeliveryConfig {
        mode: "announce".to_string(),
        channel: args.delivery_channel,
        to: args.delivery_to,
        thread_id: args.delivery_thread,
        best_effort: !args.no_best_effort,
    })
}

/// Print where a created/updated job's output will go, so an `ok` job status is
/// never mistaken for a successful delivery.
fn print_delivery_line(job: &CronJob) {
    let value = if job.delivery.mode.eq_ignore_ascii_case("announce") {
        match (job.delivery.channel.as_deref(), job.delivery.to.as_deref()) {
            (Some(channel), Some(to)) => format!("{channel} \u{2192} {to}"),
            (Some(channel), None) => channel.to_string(),
            _ => "announce".to_string(),
        }
    } else {
        get_required_cli_string("cli-cron-delivery-disabled")
    };
    println!(
        "{}",
        get_required_cli_string_with_args("cli-cron-delivery", &[("v", &value)])
    );
}

pub fn handle_command(command: crate::CronCommands, config: &Config) -> Result<()> {
    match command {
        crate::CronCommands::List => {
            let jobs = list_jobs(config)?;
            if jobs.is_empty() {
                println!("{}", get_required_cli_string("cli-cron-none"));
                println!("\n{}", get_required_cli_string("cli-cron-usage"));
                println!("  zeroclaw cron add '0 9 * * *' 'echo ok' --agent sentinel"); // i18n-exempt: literal command example
                return Ok(());
            }

            println!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-cron-jobs-header",
                    &[("count", &jobs.len().to_string())]
                )
            );
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
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-cron-list-cmd",
                            &[("cmd", &job.command)]
                        )
                    );
                }
                if let Some(prompt) = &job.prompt {
                    println!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-cron-list-prompt",
                            &[("prompt", prompt)]
                        )
                    );
                }
            }
            Ok(())
        }
        crate::CronCommands::Add {
            expression,
            agent_alias,
            tz,
            prompt,
            allowed_tools,
            uses_memory,
            delivery,
            command,
        } => {
            require_configured_agent(config, &agent_alias)?;
            let schedule = Schedule::Cron {
                expr: expression,
                tz,
            };
            let delivery = build_delivery(delivery);
            if prompt {
                let job = add_agent_job(
                    config,
                    &agent_alias,
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
                    uses_memory.unwrap_or(true),
                )?;
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-added-agent", &[("id", &job.id)])
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-expr", &[("v", &job.expression)])
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-next",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-prompt",
                        &[("v", job.prompt.as_deref().unwrap_or_default())]
                    )
                );
                print_delivery_line(&job);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --prompt cron jobs");
                }
                let job = add_shell_job_with_approval(
                    config,
                    &agent_alias,
                    None,
                    schedule,
                    &command,
                    delivery,
                    false,
                )?;
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-added", &[("id", &job.id)])
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-expr2", &[("v", &job.expression)])
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-next2",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-cmd", &[("v", &job.command)])
                );
                print_delivery_line(&job);
            }
            Ok(())
        }
        crate::CronCommands::AddAt {
            at,
            agent_alias,
            prompt,
            allowed_tools,
            uses_memory,
            delivery,
            command,
        } => {
            require_configured_agent(config, &agent_alias)?;
            let at = parse_explicit_rfc3339_utc(&at)?;
            let schedule = Schedule::At { at };
            let delivery = build_delivery(delivery);
            if prompt {
                let job = add_agent_job(
                    config,
                    &agent_alias,
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
                    uses_memory.unwrap_or(true),
                )?;
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-added-oneshot-agent",
                        &[("id", &job.id)]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-at",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-prompt",
                        &[("v", job.prompt.as_deref().unwrap_or_default())]
                    )
                );
                print_delivery_line(&job);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --prompt cron jobs");
                }
                let job = add_shell_job_with_approval(
                    config,
                    &agent_alias,
                    None,
                    schedule,
                    &command,
                    delivery,
                    false,
                )?;
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-added-oneshot", &[("id", &job.id)])
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-at2",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-cmd", &[("v", &job.command)])
                );
                print_delivery_line(&job);
            }
            Ok(())
        }
        crate::CronCommands::AddEvery {
            every_ms,
            agent_alias,
            prompt,
            allowed_tools,
            uses_memory,
            delivery,
            command,
        } => {
            require_configured_agent(config, &agent_alias)?;
            let schedule = Schedule::Every { every_ms };
            let delivery = build_delivery(delivery);
            if prompt {
                let job = add_agent_job(
                    config,
                    &agent_alias,
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
                    uses_memory.unwrap_or(true),
                )?;
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-added-interval-agent",
                        &[("id", &job.id)]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-every",
                        &[("v", &every_ms.to_string())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-next3",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-prompt3",
                        &[("v", job.prompt.as_deref().unwrap_or_default())]
                    )
                );
                print_delivery_line(&job);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --prompt cron jobs");
                }
                let job = add_shell_job_with_approval(
                    config,
                    &agent_alias,
                    None,
                    schedule,
                    &command,
                    delivery,
                    false,
                )?;
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-added-interval",
                        &[("id", &job.id)]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-every",
                        &[("v", &every_ms.to_string())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-next3",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-cmd3", &[("v", &job.command)])
                );
                print_delivery_line(&job);
            }
            Ok(())
        }
        crate::CronCommands::Once {
            delay,
            agent_alias,
            prompt,
            allowed_tools,
            uses_memory,
            delivery,
            command,
        } => {
            require_configured_agent(config, &agent_alias)?;
            let delivery = build_delivery(delivery);
            if prompt {
                let duration = parse_delay(&delay)?;
                let at = chrono::Utc::now() + duration;
                let schedule = Schedule::At { at };
                let job = add_agent_job(
                    config,
                    &agent_alias,
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
                    uses_memory.unwrap_or(true),
                )?;
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-added-oneshot-agent",
                        &[("id", &job.id)]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-at",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-prompt",
                        &[("v", job.prompt.as_deref().unwrap_or_default())]
                    )
                );
                print_delivery_line(&job);
            } else {
                if !allowed_tools.is_empty() {
                    bail!("--allowed-tool is only supported with --prompt cron jobs");
                }
                let job = add_once(config, &agent_alias, &delay, &command, delivery)?;
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-added-oneshot", &[("id", &job.id)])
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-cron-at2",
                        &[("v", &job.next_run.to_rfc3339())]
                    )
                );
                println!(
                    "{}",
                    get_required_cli_string_with_args("cli-cron-cmd", &[("v", &job.command)])
                );
                print_delivery_line(&job);
            }
            Ok(())
        }
        crate::CronCommands::Update {
            id,
            agent_alias,
            expression,
            tz,
            command,
            name,
            allowed_tools,
            uses_memory,
            delivery,
        } => {
            require_configured_agent(config, &agent_alias)?;
            let delivery = build_delivery(delivery);
            // The create paths validate delivery inside `add_*_with_approval`;
            // the update path does not, so validate here before patching.
            validate_delivery_config(delivery.as_ref())?;
            if expression.is_none()
                && tz.is_none()
                && command.is_none()
                && name.is_none()
                && allowed_tools.is_empty()
                && uses_memory.is_none()
                && delivery.is_none()
            {
                bail!("{}", get_required_cli_string("cli-cron-update-no-field"));
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
                uses_memory,
                delivery,
                ..CronJobPatch::default()
            };

            let job = update_shell_job_with_approval(config, &agent_alias, &id, patch, false)?;
            println!(
                "{}",
                get_required_cli_string_with_args("cli-cron-updated", &[("id", &job.id)])
            );
            println!(
                "{}",
                get_required_cli_string_with_args("cli-cron-expr2", &[("v", &job.expression)])
            );
            println!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-cron-next2",
                    &[("v", &job.next_run.to_rfc3339())]
                )
            );
            println!(
                "{}",
                get_required_cli_string_with_args("cli-cron-cmd", &[("v", &job.command)])
            );
            print_delivery_line(&job);
            Ok(())
        }
        crate::CronCommands::Remove { id } => {
            remove_job(config, &id)?;
            println!(
                "{}",
                get_required_cli_string_with_args("cli-cron-removed", &[("id", &id)])
            );
            Ok(())
        }
        crate::CronCommands::Pause { id } => {
            pause_job(config, &id)?;
            println!(
                "{}",
                get_required_cli_string_with_args("cli-cron-paused", &[("id", &id)])
            );
            Ok(())
        }
        crate::CronCommands::Resume { id } => {
            resume_job(config, &id)?;
            println!(
                "{}",
                get_required_cli_string_with_args("cli-cron-resumed", &[("id", &id)])
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config
            .risk_profiles
            .entry("test-agent".to_string())
            .or_default();
        config
            .runtime_profiles
            .entry("test-agent".to_string())
            .or_default();
        config
            .providers
            .models
            .ensure("openrouter", "test-agent")
            .expect("known family");
        config.agents.entry("test-agent".to_string()).or_insert(
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.test-agent".into(),
                risk_profile: "test-agent".into(),
                runtime_profile: "test-agent".into(),
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn cli_add_at_rejects_timestamp_without_explicit_offset_with_actionable_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = handle_command(
            crate::CronCommands::AddAt {
                at: "2026-05-18T09:00:00".into(),
                agent_alias: "test-agent".into(),
                prompt: false,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs::default(),
                command: "echo at".into(),
            },
            &config,
        );

        let error = result.expect_err("bare local timestamp must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("RFC3339 timestamp with explicit Z or offset"),
            "error should explain the explicit offset requirement: {message}"
        );
        assert!(message.contains("2026-05-18T09:00:00Z"));
        assert!(message.contains("2026-05-18T09:00:00-04:00"));
    }

    #[test]
    fn cli_add_persists_delivery_config() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                agent_alias: "test-agent".into(),
                tz: None,
                prompt: false,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs {
                    delivery_channel: Some("telegram".into()),
                    delivery_to: Some("12345".into()),
                    delivery_thread: None,
                    no_best_effort: true,
                },
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
    fn cli_add_with_thread_persists_thread_id() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                agent_alias: "test-agent".into(),
                tz: None,
                prompt: false,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs {
                    delivery_channel: Some("webhook".into()),
                    delivery_to: Some("hook-1".into()),
                    delivery_thread: Some("thread-9".into()),
                    no_best_effort: false,
                },
                command: "echo ok".into(),
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs[0].delivery.thread_id.as_deref(), Some("thread-9"));
        assert!(jobs[0].delivery.best_effort);
    }

    #[test]
    fn cli_add_without_delivery_flags_defaults_to_none() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                agent_alias: "test-agent".into(),
                tz: None,
                prompt: false,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs::default(),
                command: "echo ok".into(),
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs[0].delivery.mode, "none");
    }

    #[test]
    fn cli_update_patches_delivery_config() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                agent_alias: "test-agent".into(),
                tz: None,
                prompt: false,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs::default(),
                command: "echo test".into(),
            },
            &config,
        )
        .unwrap();
        let id = list_jobs(&config).unwrap()[0].id.clone();

        handle_command(
            crate::CronCommands::Update {
                id: id.clone(),
                agent_alias: "test-agent".into(),
                expression: None,
                tz: None,
                command: None,
                name: None,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs {
                    delivery_channel: Some("discord".into()),
                    delivery_to: Some("chan-42".into()),
                    delivery_thread: None,
                    no_best_effort: false,
                },
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &id).unwrap();
        assert_eq!(updated.delivery.mode, "announce");
        assert_eq!(updated.delivery.channel.as_deref(), Some("discord"));
        assert_eq!(updated.delivery.to.as_deref(), Some("chan-42"));
        assert!(updated.delivery.best_effort);
    }

    #[test]
    fn cli_update_rejects_announce_without_recipient() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                agent_alias: "test-agent".into(),
                tz: None,
                prompt: false,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs::default(),
                command: "echo test".into(),
            },
            &config,
        )
        .unwrap();
        let id = list_jobs(&config).unwrap()[0].id.clone();

        // --channel without --to is an incomplete announce config; the update
        // path must reject it rather than persist an unroutable delivery.
        let result = handle_command(
            crate::CronCommands::Update {
                id,
                agent_alias: "test-agent".into(),
                expression: None,
                tz: None,
                command: None,
                name: None,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs {
                    delivery_channel: Some("telegram".into()),
                    delivery_to: None,
                    delivery_thread: None,
                    no_best_effort: false,
                },
            },
            &config,
        );
        let err = result.expect_err("announce without --to must be rejected");
        assert!(
            err.to_string().contains("delivery.to is required"),
            "unexpected error: {err}"
        );
    }
}
