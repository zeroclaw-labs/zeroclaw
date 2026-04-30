use crate::cron::{
    CronJob, CronJobPatch, DeliveryConfig, JobType, Schedule, SessionTarget, all_overdue_jobs,
    due_jobs, next_run_for_schedule, record_last_run, record_run, remove_job, reschedule_after_run,
    sync_declarative_jobs, update_job,
};
use crate::security::SecurityPolicy;
use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{self, Duration};
use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::{CronJobDecl, CronScheduleDecl};

const MIN_POLL_SECONDS: u64 = 5;
const SHELL_JOB_TIMEOUT_SECS: u64 = 120;
const SCHEDULER_COMPONENT: &str = "scheduler";

/// Type alias for the optional broadcast sender used to push cron results
/// to connected dashboard/SSE clients.
pub type EventBroadcast = Option<tokio::sync::broadcast::Sender<serde_json::Value>>;

pub async fn run(config: Config, event_tx: EventBroadcast) -> Result<()> {
    let poll_secs = config.reliability.scheduler_poll_secs.max(MIN_POLL_SECONDS);
    let mut interval = time::interval(Duration::from_secs(poll_secs));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    crate::health::mark_component_ok(SCHEDULER_COMPONENT);

    // ── Declarative job sync: reconcile config-defined jobs with the DB.
    let mut jobs_with_builtin = config.cron.jobs.clone();
    if let Some(ref schedule_cron) = config.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: config.backup.schedule_timezone.clone(),
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            uses_memory: true,
            session_target: None,
            delivery: None,
        };
        tracing::debug!(
            schedule = %schedule_cron,
            "Synthesizing builtin backup cron job from config.backup.schedule_cron"
        );
        jobs_with_builtin.push(backup_job);
    }

    match sync_declarative_jobs(&config, &jobs_with_builtin) {
        Ok(()) => {
            if !jobs_with_builtin.is_empty() {
                tracing::info!(
                    count = jobs_with_builtin.len(),
                    "Synced declarative cron jobs from config"
                );
            }
        }
        Err(e) => tracing::warn!("Failed to sync declarative cron jobs: {e}"),
    }

    // ── Startup catch-up: run ALL overdue jobs before entering the
    //    normal polling loop. The regular loop is capped by `max_tasks`,
    //    which could leave some overdue jobs waiting across many cycles
    //    if the machine was off for a while. The catch-up phase fetches
    //    without the `max_tasks` limit so every missed job fires once.
    //    Controlled by `[cron] catch_up_on_startup` (default: true).
    if config.cron.catch_up_on_startup {
        catch_up_overdue_jobs(&config, &security, &event_tx).await;
    } else {
        tracing::info!("Scheduler startup: catch-up disabled by config");
    }

    loop {
        interval.tick().await;
        // Keep scheduler liveness fresh even when there are no due jobs.
        crate::health::mark_component_ok(SCHEDULER_COMPONENT);

        let jobs = match due_jobs(&config, Utc::now()) {
            Ok(jobs) => jobs,
            Err(e) => {
                crate::health::mark_component_error(SCHEDULER_COMPONENT, e.to_string());
                tracing::warn!("Scheduler query failed: {e}");
                continue;
            }
        };

        process_due_jobs(&config, &security, jobs, SCHEDULER_COMPONENT, &event_tx).await;
    }
}

/// Fetch **all** overdue jobs (ignoring `max_tasks`) and execute them.
///
/// Called once at scheduler startup so that jobs missed during downtime
/// (e.g. late boot, daemon restart) are caught up immediately.
async fn catch_up_overdue_jobs(
    config: &Config,
    security: &Arc<SecurityPolicy>,
    event_tx: &EventBroadcast,
) {
    let now = Utc::now();
    let jobs = match all_overdue_jobs(config, now) {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!("Startup catch-up query failed: {e}");
            return;
        }
    };

    if jobs.is_empty() {
        tracing::info!("Scheduler startup: no overdue jobs to catch up");
        return;
    }

    tracing::info!(
        count = jobs.len(),
        "Scheduler startup: catching up overdue jobs"
    );

    process_due_jobs(config, security, jobs, SCHEDULER_COMPONENT, event_tx).await;

    tracing::info!("Scheduler startup: catch-up complete");
}

pub async fn execute_job_now(config: &Config, job: &CronJob) -> (bool, String) {
    let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
    Box::pin(execute_job_with_retry(config, &security, job)).await
}

async fn execute_job_with_retry(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    let mut last_output = String::new();
    let retries = config.reliability.scheduler_retries;
    let mut backoff_ms = config.reliability.provider_backoff_ms.max(200);

    for attempt in 0..=retries {
        let (success, output) = match job.job_type {
            JobType::Shell => run_job_command(config, security, job).await,
            JobType::Agent => Box::pin(run_agent_job(config, security, job)).await,
        };
        last_output = output;

        if success {
            return (true, last_output);
        }

        if last_output.starts_with("blocked by security policy:") {
            // Deterministic policy violations are not retryable.
            return (false, last_output);
        }

        if attempt < retries {
            let jitter_ms = u64::from(Utc::now().timestamp_subsec_millis() % 250);
            time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms.saturating_mul(2)).min(30_000);
        }
    }

    (false, last_output)
}

async fn process_due_jobs(
    config: &Config,
    security: &Arc<SecurityPolicy>,
    jobs: Vec<CronJob>,
    component: &str,
    event_tx: &EventBroadcast,
) {
    // Refresh scheduler health on every successful poll cycle, including idle cycles.
    crate::health::mark_component_ok(component);

    let max_concurrent = config.scheduler.max_concurrent.max(1);
    let mut in_flight = stream::iter(jobs.into_iter().map(|job| {
        let config = config.clone();
        let security = Arc::clone(security);
        let component = component.to_owned();
        async move {
            Box::pin(execute_and_persist_job(
                &config,
                security.as_ref(),
                &job,
                &component,
            ))
            .await
        }
    }))
    .buffer_unordered(max_concurrent);

    while let Some((job_id, success, output)) = in_flight.next().await {
        if !success {
            tracing::warn!("Scheduler job '{job_id}' failed: {output}");
        }
        // Broadcast cron result to dashboard/SSE clients.
        if let Some(tx) = event_tx {
            let _ = tx.send(serde_json::json!({
                "type": "cron_result",
                "job_id": job_id,
                "success": success,
                "output": output,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
    }
}

async fn execute_and_persist_job(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    component: &str,
) -> (String, bool, String) {
    crate::health::mark_component_ok(component);
    warn_if_high_frequency_agent_job(job);

    let started_at = Utc::now();
    let (success, output) = Box::pin(execute_job_with_retry(config, security, job)).await;
    let finished_at = Utc::now();
    let success = Box::pin(persist_job_result(
        config,
        job,
        success,
        &output,
        started_at,
        finished_at,
    ))
    .await;

    (job.id.clone(), success, output)
}

async fn run_agent_job(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    if !security.can_act() {
        return (
            false,
            "blocked by security policy: autonomy is read-only".to_string(),
        );
    }

    if security.is_rate_limited() {
        return (
            false,
            "blocked by security policy: rate limit exceeded".to_string(),
        );
    }

    if !security.record_action() {
        return (
            false,
            "blocked by security policy: action budget exhausted".to_string(),
        );
    }
    let name = job.name.clone().unwrap_or_else(|| "cron-job".to_string());
    let prompt = job.prompt.clone().unwrap_or_default();

    // Recall relevant memories so cron jobs have context awareness.
    // Skipped when `job.uses_memory` is false (e.g. stateless digest jobs).
    // Exclude `Conversation` memories to prevent chat context from
    // leaking into scheduled executions (see #5415).
    let memory_context = if !job.uses_memory {
        String::new()
    } else {
        match zeroclaw_memory::create_memory(
            &config.memory,
            &config.workspace_dir,
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
        ) {
            Ok(mem) => match mem.recall(&prompt, 5, None, None, None).await {
                Ok(entries) if !entries.is_empty() => {
                    let ctx: String = entries
                        .iter()
                        .filter(|e| {
                            !matches!(
                                e.category,
                                zeroclaw_memory::traits::MemoryCategory::Conversation
                            )
                        })
                        .map(|e| format!("- {}: {}", e.key, e.content))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if ctx.is_empty() {
                        String::new()
                    } else {
                        format!("[Memory context]\n{ctx}\n[/Memory context]\n\n")
                    }
                }
                _ => String::new(),
            },
            Err(_) => String::new(),
        }
    };

    let prefixed_prompt = format!("{memory_context}[cron:{} {name}] {prompt}", job.id);
    let model_override = job.model.clone();

    let mut cron_config = config.clone();
    cron_config.memory.auto_save = false;

    // Assign a unique session ID so memories written during this run can be
    // purged atomically if the run fails (prevents snowball accumulation).
    let run_session_id = uuid::Uuid::new_v4().to_string();
    let session_path = std::path::PathBuf::from(format!("cron-{run_session_id}"));

    let run_result = match job.session_target {
        SessionTarget::Main | SessionTarget::Isolated => {
            Box::pin(crate::agent::run(
                cron_config,
                Some(prefixed_prompt),
                None,
                model_override,
                config
                    .providers
                    .fallback_provider()
                    .and_then(|e| e.temperature)
                    .unwrap_or(0.7),
                vec![],
                false,
                Some(session_path.clone()),
                job.allowed_tools.clone(),
            ))
            .await
        }
    };

    match run_result {
        Ok(response) => (
            true,
            if response.trim().is_empty() {
                "agent job executed".to_string()
            } else {
                response
            },
        ),
        Err(e) => {
            // Purge memories written during this failed run so they don't
            // pollute future recall and cause context snowball.
            let mem_session_key = format!("cli:{}", session_path.display());
            if let Ok(mem) = zeroclaw_memory::create_memory(
                &config.memory,
                &config.workspace_dir,
                config
                    .providers
                    .fallback_provider()
                    .and_then(|e| e.api_key.as_deref()),
            ) {
                let _ = mem.purge_session(&mem_session_key).await;
            }
            (false, format!("agent job failed: {e}"))
        }
    }
}

async fn persist_job_result(
    config: &Config,
    job: &CronJob,
    mut success: bool,
    output: &str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) -> bool {
    let duration_ms = (finished_at - started_at).num_milliseconds();

    if let Err(e) = deliver_if_configured(config, job, output).await {
        if job.delivery.best_effort {
            tracing::warn!("Cron delivery failed (best_effort): {e}");
        } else {
            success = false;
            tracing::warn!("Cron delivery failed: {e}");
        }
    }

    let _ = record_run(
        config,
        &job.id,
        started_at,
        finished_at,
        if success { "ok" } else { "error" },
        Some(output),
        duration_ms,
    );

    if is_one_shot_auto_delete(job) {
        if success {
            if let Err(e) = remove_job(config, &job.id) {
                tracing::warn!("Failed to remove one-shot cron job after success: {e}");
                // Fall back to disabling the job so it won't re-trigger.
                let _ = update_job(
                    config,
                    &job.id,
                    CronJobPatch {
                        enabled: Some(false),
                        ..CronJobPatch::default()
                    },
                );
            }
        } else {
            let _ = record_last_run(config, &job.id, finished_at, false, output);
            if let Err(e) = update_job(
                config,
                &job.id,
                CronJobPatch {
                    enabled: Some(false),
                    ..CronJobPatch::default()
                },
            ) {
                tracing::warn!("Failed to disable failed one-shot cron job: {e}");
            }
        }
        return success;
    }

    if let Err(e) = reschedule_after_run(config, job, success, output) {
        tracing::warn!("Failed to persist scheduler run result: {e}");
    }

    success
}

fn is_one_shot_auto_delete(job: &CronJob) -> bool {
    job.delete_after_run && matches!(job.schedule, Schedule::At { .. })
}

fn is_high_frequency_agent_job(job: &CronJob) -> bool {
    if !matches!(job.job_type, JobType::Agent) {
        return false;
    }
    match &job.schedule {
        Schedule::Every { every_ms } => *every_ms < 5 * 60 * 1000,
        Schedule::Cron { .. } => {
            let now = Utc::now();
            next_run_for_schedule(&job.schedule, now)
                .and_then(|a| next_run_for_schedule(&job.schedule, a).map(|b| (a, b)))
                .map(|(a, b)| (b - a).num_minutes() < 5)
                .unwrap_or(false)
        }
        Schedule::At { .. } => false,
    }
}

fn warn_if_high_frequency_agent_job(job: &CronJob) {
    if is_high_frequency_agent_job(job) {
        tracing::warn!(
            "Cron agent job '{}' is scheduled more frequently than every 5 minutes",
            job.id
        );
    }
}

async fn deliver_if_configured(config: &Config, job: &CronJob, output: &str) -> Result<()> {
    let delivery: &DeliveryConfig = &job.delivery;
    if !delivery.mode.eq_ignore_ascii_case("announce") {
        return Ok(());
    }

    let channel = delivery
        .channel
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delivery.channel is required for announce mode"))?;
    let target = delivery
        .to
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delivery.to is required for announce mode"))?;

    deliver_announcement(config, channel, target, output).await
}

/// Delivery function type — takes owned values so the returned future is 'static.
pub type DeliveryFn = Box<
    dyn Fn(
            Config,
            String,
            String,
            String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Global delivery function, injected by the binary crate at startup.
static DELIVERY_FN: std::sync::OnceLock<DeliveryFn> = std::sync::OnceLock::new();

/// Register the channel delivery function. Called once at startup by the binary.
pub fn register_delivery_fn(f: DeliveryFn) {
    let _ = DELIVERY_FN.set(f);
}

pub async fn deliver_announcement(
    config: &Config,
    channel: &str,
    target: &str,
    output: &str,
) -> Result<()> {
    if let Some(f) = DELIVERY_FN.get() {
        f(
            config.clone(),
            channel.to_string(),
            target.to_string(),
            output.to_string(),
        )
        .await
    } else {
        // No handler registered: this is a runtime-level state (the binary
        // hasn't called `register_delivery_fn`), not a per-job failure.
        // Returning `Err` here would force every announce-mode job to set
        // `best_effort=true` just to survive a system that legitimately has
        // no delivery wired (e.g. headless test runs, gateway-only deployments
        // where channel orchestration lives elsewhere).
        //
        // We log loudly via `tracing::warn` so operators see the dropped
        // delivery in their logs, then return `Ok(())` so `persist_job_result`
        // records the job execution itself as successful. Operators that
        // actively rely on delivery wire a handler at startup; absence is a
        // configuration signal, not a delivery error.
        tracing::warn!(
            channel = %channel,
            target = %target,
            "Cron delivery skipped: no delivery handler registered \
             (register_delivery_fn was not called by the binary)"
        );
        Ok(())
    }
}

async fn run_job_command(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    run_job_command_with_timeout(
        config,
        security,
        job,
        Duration::from_secs(SHELL_JOB_TIMEOUT_SECS),
    )
    .await
}

async fn run_job_command_with_timeout(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    timeout: Duration,
) -> (bool, String) {
    if !security.can_act() {
        return (
            false,
            "blocked by security policy: autonomy is read-only".to_string(),
        );
    }

    if security.is_rate_limited() {
        return (
            false,
            "blocked by security policy: rate limit exceeded".to_string(),
        );
    }

    // Unified command validation: allowlist + risk + path checks in one call.
    // Jobs created via the validated helpers were already checked at creation
    // time, but we re-validate at execution time to catch policy changes and
    // manually-edited job stores.
    let approved = false; // scheduler runs are never pre-approved
    if let Err(error) =
        crate::cron::validate_shell_command_with_security(security, &job.command, approved)
    {
        return (false, error.to_string());
    }

    if let Some(path) = security.forbidden_path_argument(&job.command) {
        return (
            false,
            format!("blocked by security policy: forbidden path argument: {path}"),
        );
    }

    if !security.record_action() {
        return (
            false,
            "blocked by security policy: action budget exhausted".to_string(),
        );
    }

    let child = match build_cron_shell_command(&job.command, &config.workspace_dir) {
        Ok(mut cmd) => match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return (false, format!("spawn error: {e}")),
        },
        Err(e) => return (false, format!("shell setup error: {e}")),
    };

    match time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!(
                "status={}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout.trim(),
                stderr.trim()
            );
            (output.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("spawn error: {e}")),
        Err(_) => (
            false,
            format!("job timed out after {}s", timeout.as_secs_f64()),
        ),
    }
}

/// Build a shell `Command` for cron job execution.
///
/// Uses `sh -c <command>` (non-login shell). On Windows, ZeroClaw users
/// typically have Git Bash installed which provides `sh` in PATH, and
/// cron commands are written with Unix shell syntax. The previous `-lc`
/// (login shell) flag was dropped: login shells load the full user
/// profile on every invocation which is slow and may cause side effects.
///
/// The command is configured with:
/// - `current_dir` set to the workspace
/// - `stdin` piped to `/dev/null` (no interactive input)
/// - `stdout` and `stderr` piped for capture
/// - `kill_on_drop(true)` for safe timeout handling
fn build_cron_shell_command(
    command: &str,
    workspace_dir: &std::path::Path,
) -> anyhow::Result<Command> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(workspace_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::{self, DeliveryConfig};
    use crate::security::SecurityPolicy;
    use chrono::{Duration as ChronoDuration, Utc};
    use tempfile::TempDir;
    use zeroclaw_config::schema::Config;

    async fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        config
    }

    fn test_job(command: &str) -> CronJob {
        CronJob {
            id: "test-job".into(),
            expression: "* * * * *".into(),
            schedule: crate::cron::Schedule::Cron {
                expr: "* * * * *".into(),
                tz: None,
            },
            command: command.into(),
            prompt: None,
            name: None,
            job_type: JobType::Shell,
            session_target: SessionTarget::Isolated,
            model: None,
            enabled: true,
            delivery: DeliveryConfig::default(),
            delete_after_run: false,
            allowed_tools: None,
            uses_memory: true,
            source: "imperative".into(),
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            last_output: None,
        }
    }

    fn unique_component(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    fn agent_job_with_schedule(schedule: crate::cron::Schedule) -> CronJob {
        CronJob {
            job_type: JobType::Agent,
            schedule,
            ..test_job("echo test")
        }
    }

    #[test]
    fn high_frequency_daily_cron_is_not_flagged() {
        // `0 6 * * *` fires once per day — must never warn regardless of when the check runs
        let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
            expr: "0 6 * * *".into(),
            tz: Some("America/Chicago".into()),
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_4min_cron_is_flagged() {
        let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
            expr: "*/4 * * * *".into(),
            tz: None,
        });
        assert!(is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_5min_cron_is_not_flagged() {
        // Exactly 5 minutes is acceptable (threshold is strictly less than 5)
        let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_interval_below_threshold_is_flagged() {
        let job = agent_job_with_schedule(crate::cron::Schedule::Every {
            every_ms: 4 * 60 * 1000, // 4 minutes
        });
        assert!(is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_interval_at_threshold_is_not_flagged() {
        let job = agent_job_with_schedule(crate::cron::Schedule::Every {
            every_ms: 5 * 60 * 1000, // exactly 5 minutes
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_shell_job_is_never_flagged() {
        // Shell jobs are exempt regardless of frequency
        let job = CronJob {
            job_type: JobType::Shell,
            schedule: crate::cron::Schedule::Every {
                every_ms: 60 * 1000, // 1 minute
            },
            ..test_job("echo test")
        };
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[tokio::test]
    async fn run_job_command_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo scheduler-ok");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(success);
        assert!(output.contains("scheduler-ok"));
        assert!(output.contains("status=exit status: 0"));
    }

    #[tokio::test]
    async fn run_job_command_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_scheduler_test");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("definitely_missing_file_for_scheduler_test"));
        assert!(output.contains("status=exit status:"));
    }

    #[tokio::test]
    async fn run_job_command_times_out() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["sleep".into()];
        let job = test_job("sleep 1");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) =
            run_job_command_with_timeout(&config, &security, &job, Duration::from_millis(50)).await;
        assert!(!success);
        assert!(output.contains("job timed out after"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_disallowed_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["echo".into()];
        let job = test_job("curl https://evil.example");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.to_lowercase().contains("not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat /etc/passwd");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_option_assignment_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["grep".into()];
        let job = test_job("grep --file=/etc/passwd root ./src");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_short_option_attached_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["grep".into()];
        let job = test_job("grep -f/etc/passwd root ./src");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_tilde_user_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat ~root/.ssh/id_rsa");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("~root/.ssh/id_rsa"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_input_redirection_path_bypass() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat </etc/passwd");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.to_lowercase().contains("not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.level = crate::security::AutonomyLevel::ReadOnly;
        let job = test_job("echo should-not-run");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.max_actions_per_hour = 0;
        let job = test_job("echo should-not-run");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_recovers_after_first_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        config.autonomy.allowed_commands = vec!["sh".into()];
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        tokio::fs::write(
            config.workspace_dir.join("retry-once.sh"),
            "#!/bin/sh\nif [ -f retry-ok.flag ]; then\n  echo recovered\n  exit 0\nfi\ntouch retry-ok.flag\nexit 1\n",
        )
        .await
        .unwrap();
        let job = test_job("sh ./retry-once.sh");

        let (success, output) = Box::pin(execute_job_with_retry(&config, &security, &job)).await;
        assert!(success);
        assert!(output.contains("recovered"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_exhausts_attempts() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let job = test_job("ls always_missing_for_retry_test");

        let (success, output) = Box::pin(execute_job_with_retry(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("always_missing_for_retry_test"));
    }

    #[tokio::test]
    async fn run_agent_job_returns_error_without_provider_key() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = Box::pin(run_agent_job(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("agent job failed:"));
    }

    #[tokio::test]
    async fn run_agent_job_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.level = crate::security::AutonomyLevel::ReadOnly;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = Box::pin(run_agent_job(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    #[tokio::test]
    async fn run_agent_job_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.max_actions_per_hour = 0;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = Box::pin(run_agent_job(&config, &security, &job)).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn process_due_jobs_marks_component_ok_even_when_idle() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("scheduler-idle");

        crate::health::mark_component_error(&component, "pre-existing error");
        process_due_jobs(&config, &security, Vec::new(), &component, &None).await;

        let snapshot = crate::health::snapshot_json();
        let entry = &snapshot["components"][component.as_str()];
        assert_eq!(entry["status"], "ok");
        assert!(entry["last_ok"].as_str().is_some());
        assert!(entry["last_error"].is_null());
    }

    #[tokio::test]
    async fn process_due_jobs_failure_does_not_mark_component_unhealthy() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_scheduler_component_health_test");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("scheduler-fail");

        crate::health::mark_component_ok(&component);
        process_due_jobs(&config, &security, vec![job], &component, &None).await;

        let snapshot = crate::health::snapshot_json();
        let entry = &snapshot["components"][component.as_str()];
        assert_eq!(entry["status"], "ok");
    }

    #[tokio::test]
    async fn persist_job_result_records_run_and_reschedules_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_job(&config, "*/5 * * * *", "echo ok").unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);
        let lookup = cron::get_job(&config, &job.id);
        assert!(lookup.is_err());
    }

    #[tokio::test]
    async fn persist_job_result_failure_disables_one_shot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, false, "boom", started, finished).await;
        assert!(!success);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_once_at(&config, at, "echo one-shot-shell").unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);
        let lookup = cron::get_job(&config, &job.id);
        assert!(lookup.is_err());
    }

    #[tokio::test]
    async fn persist_job_result_failure_disables_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_once_at(&config, at, "echo one-shot-shell").unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, false, "boom", started, finished).await;
        assert!(!success);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn persist_job_result_delivery_stubbed_succeeds() {
        // Delivery is stubbed (moved to zeroclaw-channels orchestrator).
        // This test verifies the stub returns Ok, so persist_job_result succeeds.
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &config,
            Some("announce-job".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "deliver this",
            SessionTarget::Isolated,
            None,
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("telegram".into()),
                to: Some("123456".into()),
                best_effort: false,
            }),
            false,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("ok"));

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn persist_job_result_delivery_failure_best_effort_keeps_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &config,
            Some("announce-job-best-effort".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "deliver this",
            SessionTarget::Isolated,
            None,
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("telegram".into()),
                to: Some("123456".into()),
                best_effort: true,
            }),
            false,
            None,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("ok"));

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn persist_job_result_at_schedule_without_delete_after_run_is_disabled() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            Some("at-no-autodelete".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(!job.delete_after_run);

        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);
        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        // After reschedule_after_run, At schedule jobs should be disabled
        // to prevent re-execution with a past next_run timestamp.
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(
            !updated.enabled,
            "At schedule job should be disabled after execution via reschedule"
        );
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn deliver_if_configured_handles_none_mode() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo ok");

        // Default delivery mode is not "announce", so should be a no-op.
        assert!(deliver_if_configured(&config, &job, "x").await.is_ok());
    }

    #[tokio::test]
    async fn deliver_announcement_returns_ok_when_no_handler_registered() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        // No registered handler is a runtime-level state, not a delivery
        // failure. The caller (persist_job_result) should record the job
        // execution as successful; the missing handler is logged via
        // tracing::warn for operator visibility.
        deliver_announcement(&config, "telegram", "chat-id", "payload")
            .await
            .expect("missing delivery handler should be Ok with a warn log");
    }

    #[test]
    fn build_cron_shell_command_uses_sh_non_login() {
        let workspace = std::env::temp_dir();
        let cmd = build_cron_shell_command("echo cron-test", &workspace).unwrap();
        let debug = format!("{cmd:?}");
        assert!(debug.contains("echo cron-test"));
        assert!(debug.contains("\"sh\""), "should use sh: {debug}");
        // Must NOT use login shell (-l) — login shells load full profile
        // and are slow/unpredictable for cron jobs.
        assert!(
            !debug.contains("\"-lc\""),
            "must not use login shell: {debug}"
        );
    }

    #[tokio::test]
    async fn build_cron_shell_command_executes_successfully() {
        let workspace = std::env::temp_dir();
        let mut cmd = build_cron_shell_command("echo cron-ok", &workspace).unwrap();
        let output = cmd.output().await.unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("cron-ok"));
    }

    #[tokio::test]
    async fn catch_up_queries_all_overdue_jobs_ignoring_max_tasks() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.scheduler.max_tasks = 1; // limit normal polling to 1

        // Create 3 jobs with "every minute" schedule
        for i in 0..3 {
            let _ = cron::add_job(&config, "* * * * *", &format!("echo catchup-{i}")).unwrap();
        }

        // Verify normal due_jobs is limited to max_tasks=1
        let far_future = Utc::now() + ChronoDuration::days(1);
        let due = cron::due_jobs(&config, far_future).unwrap();
        assert_eq!(due.len(), 1, "due_jobs must respect max_tasks");

        // all_overdue_jobs ignores the limit
        let overdue = cron::all_overdue_jobs(&config, far_future).unwrap();
        assert_eq!(overdue.len(), 3, "all_overdue_jobs must return all");
    }

    // scan_and_redact_output tests moved to zeroclaw-channels orchestrator

    // ── Broadcast / EventBroadcast tests ─────────────────────────────

    #[tokio::test]
    async fn broadcast_sends_cron_result_on_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo broadcast-ok");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-ok");

        let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, &security, vec![job], &component, &event_tx).await;

        let event = rx.try_recv().expect("should receive a broadcast event");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["job_id"], "test-job");
        assert_eq!(event["success"], true);
        assert!(event["output"].as_str().unwrap().contains("broadcast-ok"));
        assert!(event["timestamp"].as_str().is_some());
    }

    #[tokio::test]
    async fn broadcast_sends_cron_result_on_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_broadcast_fail_test");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-fail");

        let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, &security, vec![job], &component, &event_tx).await;

        let event = rx.try_recv().expect("should receive a broadcast event");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["job_id"], "test-job");
        assert_eq!(event["success"], false);
        assert!(event["timestamp"].as_str().is_some());
    }

    #[tokio::test]
    async fn broadcast_none_skips_without_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo no-broadcast");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-none");

        // event_tx = None — should complete without panic.
        process_due_jobs(&config, &security, vec![job], &component, &None).await;
    }

    #[tokio::test]
    async fn broadcast_handles_no_subscribers() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo no-subscribers");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("broadcast-no-sub");

        let (tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        // Drop the only receiver immediately — `let _ = tx.send(...)` in
        // process_due_jobs must not panic when there are no subscribers.
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, &security, vec![job], &component, &event_tx).await;
        // If we got here without panic, the test passes.
    }
}
