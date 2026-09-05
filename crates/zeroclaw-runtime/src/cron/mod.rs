use crate::security::SecurityPolicy;
use anyhow::{Result, bail};
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::schema::{Config, CronShellOutputFormat};

mod schedule;
mod store;
mod types;

pub mod scheduler;

#[allow(unused_imports)]
pub use schedule::{
    next_run_for_schedule, normalize_expression, schedule_cron_expression, validate_schedule,
};
pub(crate) use store::finish_agent_claim;
#[cfg(test)]
pub(crate) use store::force_release_failure_for_tests;
#[allow(unused_imports)]
pub use store::{
    add_agent_job, all_overdue_jobs, claim_job, claim_job_for_agent,
    claim_job_for_agent_with_token, clear_stale_locks, due_jobs, get_job, get_job_for_agent,
    list_jobs, list_jobs_by_agent, list_runs, list_runs_for_agent, record_last_run,
    record_last_run_with_status, record_run, release_job, release_job_for_token, remove_job,
    remove_job_for_agent, remove_jobs_by_agent, rename_jobs_by_agent, reschedule_after_run,
    reschedule_after_run_with_status, resolve_job_id_or_name, skip_missed_run,
    sync_declarative_jobs, update_job, update_job_for_agent,
};
pub use types::{
    CronJob, CronJobPatch, CronRun, DeliveryConfig, JobType, Schedule, SessionTarget,
    deserialize_maybe_stringified,
};

/// Channel names exposed by the cron tool schemas. Actual runtime delivery is
/// provided by the registered channel delivery handler, not this static enum.
pub(crate) const CRON_DELIVERY_SCHEMA_CHANNELS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "mattermost",
    "matrix",
    "qq",
    "whatsapp",
    "webhook",
    "lark",
    "feishu",
    "dingtalk",
    "wechat",
    "signal",
    "email",
];

/// JSON Schema `pattern` for a cron delivery channel.
///
/// Accepts either a bare channel type (`telegram`) or a configured instance's
/// composite key (`telegram.work`). The tool descriptions recommend the
/// composite form, and a bare-type enum would reject exactly what they
/// recommend — `cron_update` in particular has no other unambiguous way to
/// select one instance in a multi-instance setup.
///
/// Built from `CRON_DELIVERY_SCHEMA_CHANNELS` so the supported types stay
/// declared once.
pub(crate) fn cron_delivery_channel_pattern() -> String {
    format!(
        "^({})(\\.[A-Za-z0-9_-]+)?$",
        CRON_DELIVERY_SCHEMA_CHANNELS.join("|")
    )
}

/// Validate a shell command against an agent's security policy
/// (allowlist + risk gate). `agent_alias` names the agent under whose
/// risk profile the command will run. Returns `Ok(())` if the command
/// passes all checks, or an error describing why it was blocked.
pub fn validate_shell_command(
    config: &Config,
    agent_alias: &str,
    command: &str,
    approved: bool,
) -> Result<()> {
    let security = SecurityPolicy::for_agent(config, agent_alias)?;
    let runtime = crate::platform::create_runtime(&config.runtime)?;
    validate_shell_command_with_security(runtime.as_ref(), &security, command, approved)
}

/// Validate a shell command using an existing `SecurityPolicy` instance.
/// Preferred when the caller already holds a `SecurityPolicy` (e.g. scheduler).
pub fn validate_shell_command_with_security(
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    command: &str,
    approved: bool,
) -> Result<()> {
    security
        .validate_command_execution_for_shell(command, approved, runtime.shell_dialect())
        .map(|_| ())
        .map_err(|reason| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"reason": reason.to_string()})),
                "cron shell command rejected by security policy"
            );
            anyhow::Error::msg(format!("blocked by security policy: {reason}"))
        })
}

pub(crate) fn add_shell_job_with_runtime(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    agent_alias: &str,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
) -> Result<CronJob> {
    add_shell_job_with_runtime_and_format(
        config,
        runtime,
        security,
        agent_alias,
        name,
        schedule,
        command,
        delivery,
        approved,
        CronShellOutputFormat::default(),
    )
}

fn add_shell_job_with_runtime_and_format(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    agent_alias: &str,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
    shell_output_format: CronShellOutputFormat,
) -> Result<CronJob> {
    validate_shell_command_with_security(runtime, security, command, approved)?;
    validate_delivery_config(delivery.as_ref())?;
    store::add_shell_job_with_format(
        config,
        agent_alias,
        name,
        schedule,
        command,
        delivery,
        shell_output_format,
    )
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
    if channel.filter(|value| !value.is_empty()).is_none() {
        bail!("delivery.channel is required for announce mode");
    }

    let has_target = delivery
        .to
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if !has_target {
        bail!("delivery.to is required for announce mode");
    }

    Ok(())
}

pub fn add_shell_job_with_approval(
    config: &Config,
    agent_alias: &str,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
) -> Result<CronJob> {
    add_shell_job_with_approval_and_format(
        config,
        agent_alias,
        name,
        schedule,
        command,
        delivery,
        approved,
        CronShellOutputFormat::default(),
    )
}

/// Like `add_shell_job_with_approval` but with an explicit shell output format.
pub fn add_shell_job_with_approval_and_format(
    config: &Config,
    agent_alias: &str,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
    shell_output_format: CronShellOutputFormat,
) -> Result<CronJob> {
    let security = SecurityPolicy::for_agent(config, agent_alias)?;
    let runtime = crate::platform::create_runtime(&config.runtime)?;
    add_shell_job_with_runtime_and_format(
        config,
        runtime.as_ref(),
        &security,
        agent_alias,
        name,
        schedule,
        command,
        delivery,
        approved,
        shell_output_format,
    )
}

/// Agent jobs execute [`CronJob::prompt`], not [`CronJob::command`]. Callers
/// that only have a `command` field (CLI `--command`, some tool payloads)
/// must not persist that text on the unused command column or run shell-policy
/// validation against a natural-language prompt. Matches `PATCH /api/cron`
/// (`command.or(prompt)` on agent jobs).
fn remap_agent_command_patch(
    config: &Config,
    job_id: &str,
    mut patch: CronJobPatch,
) -> Result<CronJobPatch> {
    if patch.command.is_none() {
        return Ok(patch);
    }
    let existing = get_job(config, job_id)?;
    if existing.job_type == JobType::Agent {
        patch.prompt = patch.command.take().or(patch.prompt);
    }
    Ok(patch)
}

/// Update a job with security validation for shell-command patches.
/// Validates a new shell command against the named agent's risk profile
/// before persisting. Agent jobs remap `command` onto `prompt` first.
pub fn update_shell_job_with_approval(
    config: &Config,
    agent_alias: &str,
    job_id: &str,
    patch: CronJobPatch,
    approved: bool,
) -> Result<CronJob> {
    let patch = remap_agent_command_patch(config, job_id, patch)?;
    if patch.command.is_none() {
        return update_job(config, job_id, patch);
    }

    let security = SecurityPolicy::for_agent(config, agent_alias)?;
    let runtime = crate::platform::create_runtime(&config.runtime)?;
    // `owner: None` — this is the OPERATOR entry point, used by the gateway API
    // and the CLI. Its `agent_alias` names whose risk profile validates the
    // command, which is not necessarily the job's owner: patching an agent-type
    // job's prompt is not agent-gated at all and may omit the agent entirely.
    // Scoping here would stop an operator patching a job.
    update_shell_job_with_runtime(
        config,
        runtime.as_ref(),
        &security,
        None,
        job_id,
        patch,
        approved,
    )
}

pub(crate) fn update_shell_job_with_runtime(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    // Some(alias) for an agent-facing call, which carries the ownership test
    // into the write; None for operator callers, which have no owning agent.
    owner: Option<&str>,
    job_id: &str,
    patch: CronJobPatch,
    approved: bool,
) -> Result<CronJob> {
    let patch = remap_agent_command_patch(config, job_id, patch)?;
    if let Some(command) = patch.command.as_deref() {
        validate_shell_command_with_security(runtime, security, command, approved)?;
    }
    match owner {
        // Scoped: the ownership test travels with the write rather than being a
        // separate read the operator's rename cascade can slip between.
        Some(agent_alias) => update_job_for_agent(config, job_id, agent_alias, patch),
        None => update_job(config, job_id, patch),
    }
}

/// Create a one-shot validated shell job from a delay string (e.g. "30m").
pub fn add_once_validated(
    config: &Config,
    agent_alias: &str,
    delay: &str,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
) -> Result<CronJob> {
    let security = SecurityPolicy::for_agent(config, agent_alias)?;
    let runtime = crate::platform::create_runtime(&config.runtime)?;
    add_once_validated_with_runtime(
        config,
        runtime.as_ref(),
        &security,
        agent_alias,
        delay,
        command,
        delivery,
        approved,
    )
}

pub(crate) fn add_once_validated_with_runtime(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    agent_alias: &str,
    delay: &str,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
) -> Result<CronJob> {
    let duration = parse_delay(delay)?;
    let at = chrono::Utc::now() + duration;
    add_once_at_validated_with_runtime(
        config,
        runtime,
        security,
        agent_alias,
        at,
        command,
        delivery,
        approved,
    )
}

/// Create a one-shot validated shell job at an absolute timestamp.
pub fn add_once_at_validated(
    config: &Config,
    agent_alias: &str,
    at: chrono::DateTime<chrono::Utc>,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
) -> Result<CronJob> {
    let security = SecurityPolicy::for_agent(config, agent_alias)?;
    let runtime = crate::platform::create_runtime(&config.runtime)?;
    add_once_at_validated_with_runtime(
        config,
        runtime.as_ref(),
        &security,
        agent_alias,
        at,
        command,
        delivery,
        approved,
    )
}

pub(crate) fn add_once_at_validated_with_runtime(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    agent_alias: &str,
    at: chrono::DateTime<chrono::Utc>,
    command: &str,
    delivery: Option<DeliveryConfig>,
    approved: bool,
) -> Result<CronJob> {
    let schedule = Schedule::At { at };
    add_shell_job_with_runtime(
        config,
        runtime,
        security,
        agent_alias,
        None,
        schedule,
        command,
        delivery,
        approved,
    )
}

// Convenience wrappers for CLI paths (default approved=false).

pub fn add_shell_job(
    config: &Config,
    agent_alias: &str,
    name: Option<String>,
    schedule: Schedule,
    command: &str,
) -> Result<CronJob> {
    add_shell_job_with_approval_and_format(
        config,
        agent_alias,
        name,
        schedule,
        command,
        None,
        false,
        CronShellOutputFormat::default(),
    )
}

pub fn add_job(
    config: &Config,
    agent_alias: &str,
    expression: &str,
    command: &str,
) -> Result<CronJob> {
    let schedule = Schedule::Cron {
        expr: expression.to_string(),
        tz: None,
    };
    add_shell_job(config, agent_alias, None, schedule, command)
}

#[allow(clippy::needless_pass_by_value)]
pub fn add_once(
    config: &Config,
    agent_alias: &str,
    delay: &str,
    command: &str,
    delivery: Option<DeliveryConfig>,
) -> Result<CronJob> {
    add_once_validated(config, agent_alias, delay, command, delivery, false)
}

pub fn add_once_at(
    config: &Config,
    agent_alias: &str,
    at: chrono::DateTime<chrono::Utc>,
    command: &str,
    delivery: Option<DeliveryConfig>,
) -> Result<CronJob> {
    add_once_at_validated(config, agent_alias, at, command, delivery, false)
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

/// Pause a job the calling agent owns. The ownership test travels with the
/// write; see `store::remove_job_for_agent`.
pub fn pause_job_for_agent(config: &Config, id: &str, agent_alias: &str) -> Result<CronJob> {
    update_job_for_agent(
        config,
        id,
        agent_alias,
        CronJobPatch {
            enabled: Some(false),
            ..CronJobPatch::default()
        },
    )
}

/// Resume a job the calling agent owns.
pub fn resume_job_for_agent(config: &Config, id: &str, agent_alias: &str) -> Result<CronJob> {
    update_job_for_agent(
        config,
        id,
        agent_alias,
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

#[cfg(test)]
mod security_validation_tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config
    }

    #[test]
    fn update_security_allows_safe_command() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let security = SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            &config.data_dir,
        );
        assert!(security.is_command_allowed("echo safe"));
    }

    #[test]
    fn scheduler_path_validates_shell_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .level = crate::security::AutonomyLevel::Supervised;

        let security = SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            &config.data_dir,
        );
        let runtime = crate::platform::create_runtime(&config.runtime).unwrap();
        // Simulate scheduler validation path
        let result = validate_shell_command_with_security(
            runtime.as_ref(),
            &security,
            "curl https://example.com",
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
}

#[cfg(test)]
mod validate_delivery_tests {
    use super::*;
    use crate::cron::types::DeliveryConfig;

    #[test]
    fn validate_delivery_accepts_webhook_with_thread_id() {
        let delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: Some("webhook".into()),
            to: Some("user-42".into()),
            thread_id: Some("conv-99".into()),
            best_effort: true,
        };
        validate_delivery_config(Some(&delivery)).expect("webhook with thread_id must validate");
    }

    #[test]
    fn validate_delivery_accepts_webhook_without_thread_id() {
        let delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: Some("webhook".into()),
            to: Some("user-42".into()),
            thread_id: None,
            best_effort: true,
        };
        validate_delivery_config(Some(&delivery)).expect("webhook without thread_id must validate");
    }
}

#[cfg(test)]
mod remap_agent_command_tests {
    use super::*;
    use crate::security::AutonomyLevel;
    use tempfile::TempDir;

    const TEST_AGENT: &str = "test-agent";

    fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config
            .risk_profiles
            .entry(TEST_AGENT.to_string())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config
            .risk_profiles
            .entry(TEST_AGENT.to_string())
            .or_default()
            .level = AutonomyLevel::Supervised;
        config
            .runtime_profiles
            .entry(TEST_AGENT.to_string())
            .or_default();
        config
            .providers
            .models
            .ensure("openrouter", TEST_AGENT)
            .expect("known family");
        config.agents.entry(TEST_AGENT.to_string()).or_insert(
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: format!("openrouter.{TEST_AGENT}").into(),
                risk_profile: TEST_AGENT.into(),
                runtime_profile: TEST_AGENT.into(),
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn update_maps_command_patch_onto_agent_prompt() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_agent_job(
            &config,
            TEST_AGENT,
            None,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "old prompt",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap();

        let updated = update_shell_job_with_approval(
            &config,
            TEST_AGENT,
            &job.id,
            CronJobPatch {
                command: Some("summarize the overnight logs".into()),
                ..CronJobPatch::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(
            updated.prompt.as_deref(),
            Some("summarize the overnight logs")
        );
        assert_eq!(updated.command, "");
        assert_eq!(updated.job_type, JobType::Agent);
    }

    #[test]
    fn update_keeps_shell_command_patches_on_the_command_column() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_job(&config, TEST_AGENT, "*/5 * * * *", "echo old").unwrap();

        let updated = update_shell_job_with_approval(
            &config,
            TEST_AGENT,
            &job.id,
            CronJobPatch {
                command: Some("echo new".into()),
                ..CronJobPatch::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(updated.command, "echo new");
        assert_eq!(updated.prompt, None);
        assert_eq!(updated.job_type, JobType::Shell);
    }

    #[test]
    fn update_does_not_run_shell_policy_against_an_agent_prompt() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_agent_job(
            &config,
            TEST_AGENT,
            None,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "old prompt",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap();

        let updated = update_shell_job_with_approval(
            &config,
            TEST_AGENT,
            &job.id,
            CronJobPatch {
                command: Some("curl https://example.com".into()),
                ..CronJobPatch::default()
            },
            false,
        )
        .expect("agent prompt text must not be validated as a shell command");

        assert_eq!(updated.prompt.as_deref(), Some("curl https://example.com"));
        assert_eq!(updated.command, "");
    }

    #[test]
    fn update_still_blocks_disallowed_shell_command_patches() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_job(&config, TEST_AGENT, "*/5 * * * *", "echo old").unwrap();

        let err = update_shell_job_with_approval(
            &config,
            TEST_AGENT,
            &job.id,
            CronJobPatch {
                command: Some("curl https://example.com".into()),
                ..CronJobPatch::default()
            },
            false,
        )
        .expect_err("shell jobs must still validate --command against policy");

        assert!(
            err.to_string().contains("blocked by security policy"),
            "unexpected error: {err}"
        );
        let unchanged = get_job(&config, &job.id).unwrap();
        assert_eq!(unchanged.command, "echo old");
    }

    #[test]
    fn update_name_only_leaves_agent_prompt_untouched() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = add_agent_job(
            &config,
            TEST_AGENT,
            None,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "keep this prompt",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap();

        let updated = update_shell_job_with_approval(
            &config,
            TEST_AGENT,
            &job.id,
            CronJobPatch {
                name: Some("morning-digest".into()),
                ..CronJobPatch::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(updated.name.as_deref(), Some("morning-digest"));
        assert_eq!(updated.prompt.as_deref(), Some("keep this prompt"));
        assert_eq!(updated.command, "");
    }
}
