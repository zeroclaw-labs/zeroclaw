pub use zeroclaw_cron::*;

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

/// Build a `DeliveryConfig` for a newly created job from the shared CLI flags.
///
/// Returns `None` (leaving the job's delivery mode `"none"`) when no delivery
/// flag is set, so omitting the flags keeps the pre-existing behaviour. Any flag
/// present builds an `"announce"` config, which `validate_delivery_config` (called
/// by the create paths) then checks for a channel and recipient. That includes
/// `--no-best-effort` on its own: it is a delivery request with no destination,
/// so it is rejected rather than silently dropped.
fn build_delivery(args: &crate::CronDeliveryArgs) -> Option<DeliveryConfig> {
    if !args.any_set() {
        return None;
    }
    Some(DeliveryConfig {
        mode: "announce".to_string(),
        channel: args.delivery_channel.clone(),
        to: args.delivery_to.clone(),
        thread_id: args.delivery_thread.clone(),
        best_effort: args.best_effort_override().unwrap_or(true),
    })
}

/// Apply the delivery flags as a patch over a job's stored delivery config.
///
/// `cron update` documents that only the fields you name change, so the delivery
/// flags follow that contract: an omitted field keeps its stored value. Changing
/// a channel therefore no longer clears an existing thread id or resets the
/// best-effort policy, and `--to` or `--thread` alone can amend a destination
/// without restating it.
///
/// Only a job already in `"announce"` mode has fields worth preserving. When
/// delivery was off, a stale channel or recipient left in the stored row must not
/// be resurrected by an unrelated flag, so the merge starts from an empty config
/// and the caller has to name a destination.
fn merge_delivery(
    existing: &DeliveryConfig,
    args: &crate::CronDeliveryArgs,
) -> Option<DeliveryConfig> {
    if !args.any_set() {
        return None;
    }
    let base = if existing.mode.eq_ignore_ascii_case("announce") {
        existing.clone()
    } else {
        DeliveryConfig::default()
    };
    Some(DeliveryConfig {
        mode: "announce".to_string(),
        channel: args.delivery_channel.clone().or(base.channel),
        to: args.delivery_to.clone().or(base.to),
        thread_id: args.delivery_thread.clone().or(base.thread_id),
        best_effort: args.best_effort_override().unwrap_or(base.best_effort),
    })
}

/// Confirm the payload the scheduler will actually run after `cron update`.
///
/// Agent jobs store that payload in `prompt` (including `--command` remaps),
/// while shell jobs keep it in `command`. Printing the unused column after a
/// successful remap makes the confirmation contradict the stored change.
fn cron_update_payload_confirmation(
    job_type: &JobType,
    command: &str,
    prompt: Option<&str>,
) -> String {
    match job_type {
        JobType::Agent => get_required_cli_string_with_args(
            "cli-cron-prompt",
            &[("v", prompt.unwrap_or_default())],
        ),
        JobType::Shell => get_required_cli_string_with_args("cli-cron-cmd", &[("v", command)]),
    }
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
            let delivery = build_delivery(&delivery);
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
            let delivery = build_delivery(&delivery);
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
            let delivery = build_delivery(&delivery);
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
            let delivery = build_delivery(&delivery);
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
            let delivery_requested = delivery.any_set();
            if expression.is_none()
                && tz.is_none()
                && command.is_none()
                && name.is_none()
                && allowed_tools.is_empty()
                && uses_memory.is_none()
                && !delivery_requested
            {
                bail!("{}", get_required_cli_string("cli-cron-update-no-field"));
            }

            let existing = if expression.is_some()
                || tz.is_some()
                || !allowed_tools.is_empty()
                || delivery_requested
            {
                Some(get_job(config, &id)?)
            } else {
                None
            };

            // Delivery flags are a patch over the stored config, matching this
            // command's contract that only the fields you name change. The
            // create paths validate inside `add_*_with_approval`; the update
            // path does not, so validate the merged result here.
            let delivery = if delivery_requested {
                let existing = existing
                    .as_ref()
                    .expect("existing job must be loaded when updating delivery");
                let merged = merge_delivery(&existing.delivery, &delivery);
                validate_delivery_config(merged.as_ref())?;
                merged
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
                cron_update_payload_confirmation(
                    &job.job_type,
                    &job.command,
                    job.prompt.as_deref()
                )
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
                    best_effort: false,
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
                    best_effort: false,
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
                    best_effort: false,
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
                    best_effort: false,
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

    /// Delivery flags on `update` are a patch, not a replacement: changing the
    /// destination must not silently clear the thread id or reset the
    /// best-effort policy the job already had.
    #[test]
    fn cli_update_preserves_unspecified_delivery_fields() {
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
                    delivery_to: Some("111".into()),
                    delivery_thread: Some("t-1".into()),
                    no_best_effort: true,
                    best_effort: false,
                },
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
                    delivery_to: Some("222".into()),
                    delivery_thread: None,
                    no_best_effort: false,
                    best_effort: false,
                },
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &id).unwrap();
        assert_eq!(updated.delivery.channel.as_deref(), Some("discord"));
        assert_eq!(updated.delivery.to.as_deref(), Some("222"));
        assert_eq!(
            updated.delivery.thread_id.as_deref(),
            Some("t-1"),
            "thread id must survive a channel/recipient change"
        );
        assert!(
            !updated.delivery.best_effort,
            "best_effort=false must survive a channel/recipient change"
        );
    }

    /// `--thread` alone amends the destination of an announcing job without
    /// restating the channel and recipient.
    #[test]
    fn cli_update_thread_alone_patches_existing_delivery() {
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
                    delivery_thread: None,
                    no_best_effort: false,
                    best_effort: false,
                },
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
                    delivery_channel: None,
                    delivery_to: None,
                    delivery_thread: Some("t-99".into()),
                    no_best_effort: false,
                    best_effort: false,
                },
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &id).unwrap();
        assert_eq!(updated.delivery.channel.as_deref(), Some("webhook"));
        assert_eq!(updated.delivery.to.as_deref(), Some("hook-1"));
        assert_eq!(updated.delivery.thread_id.as_deref(), Some("t-99"));
    }

    /// `--no-best-effort` alone is a real request. It must reach the job rather
    /// than being read as "no delivery flags given".
    #[test]
    fn cli_update_no_best_effort_alone_patches_policy() {
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
                    delivery_to: Some("111".into()),
                    delivery_thread: None,
                    no_best_effort: false,
                    best_effort: false,
                },
                command: "echo test".into(),
            },
            &config,
        )
        .unwrap();
        let id = list_jobs(&config).unwrap()[0].id.clone();
        assert!(get_job(&config, &id).unwrap().delivery.best_effort);

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
                    delivery_channel: None,
                    delivery_to: None,
                    delivery_thread: None,
                    no_best_effort: true,
                    best_effort: false,
                },
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &id).unwrap();
        assert!(!updated.delivery.best_effort);
        assert_eq!(
            updated.delivery.channel.as_deref(),
            Some("telegram"),
            "destination must be preserved"
        );
    }

    /// `--best-effort` restores the default after a `--no-best-effort`, so the
    /// policy is reversible from the CLI.
    #[test]
    fn cli_update_best_effort_restores_default() {
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
                    delivery_to: Some("111".into()),
                    delivery_thread: None,
                    no_best_effort: true,
                    best_effort: false,
                },
                command: "echo test".into(),
            },
            &config,
        )
        .unwrap();
        let id = list_jobs(&config).unwrap()[0].id.clone();
        assert!(!get_job(&config, &id).unwrap().delivery.best_effort);

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
                    delivery_channel: None,
                    delivery_to: None,
                    delivery_thread: None,
                    no_best_effort: false,
                    best_effort: true,
                },
            },
            &config,
        )
        .unwrap();

        assert!(get_job(&config, &id).unwrap().delivery.best_effort);
    }

    /// `--no-best-effort` on create is a delivery request with no destination.
    /// It must fail validation rather than be silently dropped.
    #[test]
    fn cli_add_no_best_effort_without_destination_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                agent_alias: "test-agent".into(),
                tz: None,
                prompt: false,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs {
                    delivery_channel: None,
                    delivery_to: None,
                    delivery_thread: None,
                    no_best_effort: true,
                    best_effort: false,
                },
                command: "echo ok".into(),
            },
            &config,
        );

        let err = result.expect_err("--no-best-effort with no destination must be rejected");
        assert!(
            err.to_string().contains("delivery.channel is required"),
            "unexpected error: {err}"
        );
        assert!(
            list_jobs(&config).unwrap().is_empty(),
            "no job should have been created"
        );
    }

    /// A job with delivery off has nothing worth preserving, so a stale channel
    /// left in the stored row must not be resurrected by an unrelated flag.
    #[test]
    fn cli_update_does_not_resurrect_disabled_delivery() {
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
        assert_eq!(get_job(&config, &id).unwrap().delivery.mode, "none");

        let result = handle_command(
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
                    delivery_channel: None,
                    delivery_to: None,
                    delivery_thread: Some("t-1".into()),
                    no_best_effort: false,
                    best_effort: false,
                },
            },
            &config,
        );

        let err = result.expect_err("thread alone cannot enable delivery from scratch");
        assert!(
            err.to_string().contains("delivery.channel is required"),
            "unexpected error: {err}"
        );
        assert_eq!(
            get_job(&config, &id).unwrap().delivery.mode,
            "none",
            "rejected update must leave the stored job untouched"
        );
    }

    #[test]
    fn cli_update_command_rewrites_agent_prompt_not_unused_command_column() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                agent_alias: "test-agent".into(),
                tz: None,
                prompt: true,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs::default(),
                command: "old prompt".into(),
            },
            &config,
        )
        .unwrap();
        let id = list_jobs(&config).unwrap()[0].id.clone();
        let created = get_job(&config, &id).unwrap();
        assert_eq!(created.job_type, JobType::Agent);
        assert_eq!(created.prompt.as_deref(), Some("old prompt"));
        assert_eq!(created.command, "");

        handle_command(
            crate::CronCommands::Update {
                id: id.clone(),
                agent_alias: "test-agent".into(),
                expression: None,
                tz: None,
                command: Some("new overnight digest".into()),
                name: None,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs::default(),
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &id).unwrap();
        assert_eq!(updated.prompt.as_deref(), Some("new overnight digest"));
        assert_eq!(
            updated.command, "",
            "agent jobs must not persist --command on the unused command column"
        );
        assert_eq!(
            cron_update_payload_confirmation(
                &updated.job_type,
                &updated.command,
                updated.prompt.as_deref(),
            ),
            get_required_cli_string_with_args("cli-cron-prompt", &[("v", "new overnight digest")]),
            "update confirmation must show the remapped prompt, not the empty command column"
        );
        assert_ne!(
            cron_update_payload_confirmation(
                &updated.job_type,
                &updated.command,
                updated.prompt.as_deref(),
            ),
            get_required_cli_string_with_args("cli-cron-cmd", &[("v", "")])
        );
    }

    #[test]
    fn cli_update_command_still_rewrites_shell_command() {
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
                command: "echo old".into(),
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
                command: Some("echo new".into()),
                name: None,
                allowed_tools: vec![],
                uses_memory: None,
                delivery: crate::CronDeliveryArgs::default(),
            },
            &config,
        )
        .unwrap();

        let updated = get_job(&config, &id).unwrap();
        assert_eq!(updated.job_type, JobType::Shell);
        assert_eq!(updated.command, "echo new");
        assert_eq!(updated.prompt, None);
        assert_eq!(
            cron_update_payload_confirmation(
                &updated.job_type,
                &updated.command,
                updated.prompt.as_deref(),
            ),
            get_required_cli_string_with_args("cli-cron-cmd", &[("v", "echo new")]),
            "shell update confirmation must keep printing the command column"
        );
    }

    #[test]
    fn cli_update_confirmation_renders_prompt_for_agent_jobs() {
        let line =
            cron_update_payload_confirmation(&JobType::Agent, "", Some("new overnight digest"));
        assert_eq!(
            line,
            get_required_cli_string_with_args("cli-cron-prompt", &[("v", "new overnight digest")])
        );
        assert_ne!(
            line,
            get_required_cli_string_with_args("cli-cron-cmd", &[("v", "")]),
            "agent confirmation must not fall through to an empty cli-cron-cmd line"
        );
    }

    #[test]
    fn cli_update_confirmation_renders_command_for_shell_jobs() {
        let line = cron_update_payload_confirmation(&JobType::Shell, "echo new", None);
        assert_eq!(
            line,
            get_required_cli_string_with_args("cli-cron-cmd", &[("v", "echo new")])
        );
    }
}
