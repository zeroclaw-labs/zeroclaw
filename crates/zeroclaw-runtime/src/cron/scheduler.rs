use crate::cron::store::{
    RunCompletionAction, persist_manual_run_result, persist_run_completion_state,
    persist_run_result,
};
use crate::cron::{
    CronJob, DeliveryConfig, JobType, Schedule, SessionTarget, all_overdue_jobs, claim_job,
    clear_stale_locks, due_jobs, next_run_for_schedule, release_job, skip_missed_run,
    sync_declarative_jobs,
};
use crate::security::SecurityPolicy;
use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::{CronJobDecl, CronScheduleDecl, CronShellOutputFormat};
use zeroclaw_log::Instrument;

const MIN_POLL_SECONDS: u64 = 5;
const SHELL_JOB_TIMEOUT_SECS: u64 = 120;
// Far higher than the shell cap because multi-turn agent runs with tool calls
// legitimately run long; this bound exists to free the in-flight lock on a hung
// run, not to police normal agent runtime.
const AGENT_JOB_TIMEOUT_SECS: u64 = 1800;
const AGENT_JOB_TIMEOUT_PREFIX: &str = "agent job timed out after ";
// `purge_isolated_session`'s best-effort backend call (subprocess, network,
// or lock-contended local store) gets its own short deadline so a stalled
// backend can never delay `execute_and_persist_job`'s persist/`release_job`
// critical path. Cleanup is still attempted; exceeding this deadline
// abandons it (logged) rather than blocking the unbounded lock-release path.
const ISOLATED_SESSION_PURGE_TIMEOUT: Duration = Duration::from_secs(3);
// Announcement delivery is awaited by `persist_job_result` *before*
// `execute_and_persist_job` calls `release_job`, so a channel send that never
// completes (dead socket, wedged provider, unresponsive HTTP endpoint) would
// otherwise hold `locked_at` set forever — even after the agent run and its
// cleanup both returned under their own deadlines. Timeout output is
// operator-visible rather than a quiet `NO_REPLY`, so announce-mode jobs
// reliably reach this path. Generous enough for a slow-but-live channel,
// finite so the claim is always released.
const CRON_DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);
const SCHEDULER_COMPONENT: &str = "scheduler";
const CRON_AGENT_DEFAULT_EXCLUDED_TOOLS: &[&str] = &[
    "cron_add",
    "cron_update",
    "cron_remove",
    "cron_run",
    "schedule",
];

// Test-only seam: lets a scheduler test shorten `CRON_DELIVERY_TIMEOUT` so the
// never-ready-delivery regression runs in milliseconds instead of the
// production 30s. Unset in production and in every test that doesn't
// explicitly scope it; `try_with` then falls back to the real constant.
#[cfg(test)]
tokio::task_local! {
    static TEST_DELIVERY_TIMEOUT: Duration;
}

// Test-only seam: simulates the synchronous, pre-await work that a real
// `agent::run` can perform before it ever yields — most importantly the
// PostgreSQL memory backend, which spawns an initializer thread and
// immediately `join()`s it while that thread connects, initializes schema and
// runs migrations. Blocking the thread (rather than sleeping asynchronously)
// is the whole point: it reproduces a future that cannot observe a timer.
#[cfg(test)]
tokio::task_local! {
    static TEST_PRE_RUN_BLOCK: Duration;
}

/// The deadline applied to announcement delivery while the job claim is held.
fn delivery_timeout() -> Duration {
    #[cfg(test)]
    if let Ok(d) = TEST_DELIVERY_TIMEOUT.try_with(|d| *d) {
        return d;
    }
    CRON_DELIVERY_TIMEOUT
}

/// Type alias for the optional broadcast sender used to push cron results
/// to connected dashboard/SSE clients.
pub type EventBroadcast = Option<tokio::sync::broadcast::Sender<serde_json::Value>>;

#[must_use]
pub fn is_no_reply_sentinel(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.eq_ignore_ascii_case("NO_REPLY") {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    // Legacy form (`NO_REPLY: ...`) is documented as "treated as INFO".
    if lower.starts_with("no_reply:") {
        return true;
    }
    // Kinded form (`NO_REPLY[KIND]: ...`): only the informational kind is a
    // "nothing to report" sentinel. REFUSE / FAIL (and any other/unknown kind)
    // carry operator-visible meaning and must be delivered, not suppressed.
    if let Some(rest) = lower.strip_prefix("no_reply[") {
        if let Some((kind, _)) = rest.split_once(']') {
            return kind.trim() == "info";
        }
        // Malformed `NO_REPLY[...` with no closing bracket: not a clean
        // sentinel — deliver it rather than guess.
        return false;
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceDecision {
    /// Send the output to the configured channel.
    Deliver,
    /// Suppress delivery: the output is a quiet `NO_REPLY` sentinel.
    SuppressNoReply,
}

impl AnnounceDecision {
    /// True when the announcement should actually be sent to the channel.
    #[must_use]
    pub fn should_deliver(self) -> bool {
        matches!(self, AnnounceDecision::Deliver)
    }
}

/// Decide whether an announce-mode output should be delivered or suppressed.
/// Suppresses only the *quiet* `NO_REPLY` forms (see [`is_no_reply_sentinel`]);
/// failure/refusal kinds and all real content are delivered.
#[must_use]
pub fn announce_delivery_decision(output: &str) -> AnnounceDecision {
    if is_no_reply_sentinel(output) {
        AnnounceDecision::SuppressNoReply
    } else {
        AnnounceDecision::Deliver
    }
}

#[derive(Clone, Copy)]
pub enum CronDeliveryContext {
    Scheduled,
    ToolManual,
    GatewayManual,
    RpcManual,
}

impl CronDeliveryContext {
    fn failure_message(self, best_effort: bool) -> &'static str {
        match (self, best_effort) {
            (Self::Scheduled, true) => "Cron delivery failed (best_effort)",
            (Self::Scheduled, false) => "Cron delivery failed",
            (Self::ToolManual, true) => "cron_run delivery failed (best_effort)",
            (Self::ToolManual, false) => "cron_run delivery failed",
            (Self::GatewayManual, true) => "manual cron trigger delivery failed (best_effort)",
            (Self::GatewayManual, false) => "manual cron trigger delivery failed",
            (Self::RpcManual, true) => "RPC cron trigger delivery failed (best_effort)",
            (Self::RpcManual, false) => "RPC cron trigger delivery failed",
        }
    }
}

pub struct ManualCronRunResult {
    pub job_id: String,
    pub success: bool,
    pub status: String,
    pub output: String,
    pub duration_ms: i64,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

pub struct CronDeliveryOutcome {
    pub success: bool,
    pub status: String,
    pub output: String,
}

pub async fn deliver_and_classify_run_result(
    config: &Config,
    job: &CronJob,
    mut success: bool,
    mut output: String,
    context: CronDeliveryContext,
) -> CronDeliveryOutcome {
    let mut status = if success { "ok" } else { "error" }.to_string();

    // Bound delivery: this future is awaited while the scheduler still holds
    // the job's in-flight claim (see `CRON_DELIVERY_TIMEOUT`). It runs on its
    // own task for the same reason the agent run does — a `DeliveryFn` that
    // blocks before its first await (DNS resolution, a synchronous client
    // build) would starve an inline timer and defeat the deadline. A stalled
    // send is then classified exactly like any other delivery failure, so
    // `best_effort` still decides whether the run degrades or errors.
    let delivery_result = {
        let d_config = config.clone();
        let d_job = job.clone();
        let d_output = output.clone();
        let deadline = delivery_timeout();
        let handle = ::zeroclaw_spawn::spawn!(async move {
            deliver_if_configured(&d_config, &d_job, &d_output).await
        });
        let abort = handle.abort_handle();
        match time::timeout(deadline, handle).await {
            Ok(Ok(res)) => res,
            Ok(Err(join_err)) => Err(anyhow::Error::msg(format!(
                "delivery task failed to complete: {join_err}"
            ))),
            Err(_) => {
                // Abandon the send so it cannot outlive the claim it was
                // blocking; the run result is still recorded and released.
                abort.abort();
                Err(anyhow::Error::msg(format!(
                    "delivery exceeded {:?} deadline while the job lock was held; abandoned",
                    deadline
                )))
            }
        }
    };

    if let Err(e) = delivery_result {
        // Cron add-time accepts dangling delivery refs (the job's channel
        // may not be provisioned yet); the loudly-logged warn here is
        // the scheduler-side half of that contract. Manual trigger paths
        // share this classifier so status history cannot drift again.
        let channel = job.delivery.channel.as_deref().unwrap_or("");
        let target = job.delivery.to.as_deref().unwrap_or("");
        let delivery_error = e.to_string();

        if job.delivery.best_effort {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "job_id": job.id,
                        "agent_alias": job.agent_alias,
                        "channel": channel,
                        "target": target,
                        "error": delivery_error
                    })),
                context.failure_message(true)
            );
            if success {
                status = "degraded".to_string();
            }
        } else {
            success = false;
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "job_id": job.id,
                        "agent_alias": job.agent_alias,
                        "channel": channel,
                        "target": target,
                        "error": delivery_error
                    })),
                context.failure_message(false)
            );
            status = "error".to_string();
        }

        if output.trim().is_empty() {
            output = format!("delivery failed: {delivery_error}");
        } else {
            output.push_str("\n\ndelivery failed: ");
            output.push_str(&delivery_error);
        }
    }

    CronDeliveryOutcome {
        success,
        status,
        output,
    }
}

pub async fn run_manual_job(
    config: &Config,
    job: &CronJob,
    context: CronDeliveryContext,
    event_tx: &EventBroadcast,
) -> ManualCronRunResult {
    let started_at = Utc::now();
    let (success, output) = execute_job_now(config, job).await;
    let finished_at = Utc::now();
    let duration_ms = (finished_at - started_at).num_milliseconds();
    let outcome = deliver_and_classify_run_result(config, job, success, output, context).await;

    if let Err(e) = persist_manual_run_result(
        config,
        job,
        started_at,
        finished_at,
        &outcome.status,
        Some(&outcome.output),
        duration_ms,
    ) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"job_id": job.id, "error": format!("{}", e)})),
            "manual cron trigger: failed to persist run history"
        );
    }

    if let Some(tx) = event_tx {
        let _ = tx.send(serde_json::json!({
            "type": "cron_result",
            "job_id": job.id,
            "success": outcome.success,
            "output": &outcome.output,
            "manual": true,
            "timestamp": finished_at.to_rfc3339(),
        }));
    }

    ManualCronRunResult {
        job_id: job.id.clone(),
        success: outcome.success,
        status: outcome.status,
        output: outcome.output,
        duration_ms,
        started_at,
        finished_at,
    }
}

pub async fn run(
    config: Config,
    event_tx: EventBroadcast,
    cancel: CancellationToken,
) -> Result<()> {
    let poll_secs = config.reliability.scheduler_poll_secs.max(MIN_POLL_SECONDS);
    let mut interval = time::interval(Duration::from_secs(poll_secs));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    crate::health::mark_component_ok(SCHEDULER_COMPONENT);

    // ── Declarative job sync: reconcile config-defined jobs with the DB.
    let mut jobs_with_builtin = config.cron.clone();
    if let Some(ref schedule_cron) = config.backup.schedule_cron {
        let backup_job = CronJobDecl {
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
            shell_output_format: CronShellOutputFormat::default(),
        };
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"schedule": schedule_cron})),
            "Synthesizing builtin backup cron job from config.backup.schedule_cron"
        );
        jobs_with_builtin.insert("__builtin_backup".to_string(), backup_job);
    }

    match sync_declarative_jobs(&config, &jobs_with_builtin) {
        Ok(()) => {
            if !jobs_with_builtin.is_empty() {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"count": jobs_with_builtin.len()})),
                    "Synced declarative cron jobs from config"
                );
            }
        }
        Err(e) => ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "Failed to sync declarative cron jobs"
        ),
    }

    // ── Stale-lock recovery: any in-flight lock present at boot was left by a
    //    run that died with the previous process. Clear it so those jobs are
    //    eligible again instead of being wedged out of `due_jobs` forever.
    match clear_stale_locks(&config) {
        Ok(0) => {}
        Ok(cleared) => ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"cleared": cleared})),
            "Cleared stale cron in-flight locks at startup"
        ),
        Err(e) => ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "Failed to clear stale cron in-flight locks at startup"
        ),
    }

    if config.scheduler.catch_up_on_startup {
        catch_up_overdue_jobs(&config, &event_tx).await;
    } else {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Scheduler startup: catch-up disabled by config"
        );
        skip_missed_jobs_on_startup(&config).await;
    }

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Keep scheduler liveness fresh even when there are no due jobs.
                crate::health::mark_component_ok(SCHEDULER_COMPONENT);

                let jobs = match due_jobs(&config, Utc::now()) {
                    Ok(jobs) => jobs,
                    Err(e) => {
                        crate::health::mark_component_error(SCHEDULER_COMPONENT, e.to_string());
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Scheduler query failed"
                        );
                        continue;
                    }
                };

                let jobs = claim_due_jobs(&config, jobs);
                process_due_jobs(&config, jobs, SCHEDULER_COMPONENT, &event_tx).await;
            }
            _ = cancel.cancelled() => {
                crate::health::mark_component_ok(SCHEDULER_COMPONENT);
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Cron scheduler shutting down via cancellation token"
                );
                return Ok(());
            }
        }
    }
}

fn resolve_owning_agent<'a>(config: &'a Config, job: &CronJob) -> Option<&'a str> {
    if !job.agent_alias.is_empty()
        && let Some((alias, _)) = config
            .agents
            .iter()
            .find(|(alias, _)| alias.as_str() == job.agent_alias)
    {
        return Some(alias.as_str());
    }
    config.agent_for_cron_job(&job.id)
}

/// Fetch **all** overdue jobs (ignoring `max_tasks`) and execute them.
/// Called once at scheduler startup so that jobs missed during downtime
/// (e.g. late boot, daemon restart) are caught up immediately.
async fn catch_up_overdue_jobs(config: &Config, event_tx: &EventBroadcast) {
    let now = Utc::now();
    let jobs = match all_overdue_jobs(config, now) {
        Ok(jobs) => jobs,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Startup catch-up query failed"
            );
            return;
        }
    };

    if jobs.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Scheduler startup: no overdue jobs to catch up"
        );
        return;
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"count": jobs.len()})),
        "Scheduler startup: catching up overdue jobs"
    );

    let jobs = claim_due_jobs(config, jobs);
    process_due_jobs(config, jobs, SCHEDULER_COMPONENT, event_tx).await;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Scheduler startup: catch-up complete"
    );
}

async fn skip_missed_jobs_on_startup(config: &Config) {
    let now = Utc::now();
    let jobs = match all_overdue_jobs(config, now) {
        Ok(jobs) => jobs,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Scheduler startup skip: query failed",
            );
            return;
        }
    };

    if jobs.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Scheduler startup skip: no overdue jobs to advance",
        );
        return;
    }

    let mut skipped_recurring: u64 = 0;
    let mut skipped_oneshot: u64 = 0;

    for job in &jobs {
        let is_oneshot = matches!(job.schedule, Schedule::At { .. });
        match skip_missed_run(config, job, now) {
            Ok(()) => {
                if is_oneshot {
                    skipped_oneshot += 1;
                } else {
                    skipped_recurring += 1;
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "job_id": job.id,
                            "error": format!("{}", e),
                        })),
                    "Scheduler startup skip: failed to advance job",
                );
            }
        }
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "total": jobs.len(),
                "skipped_recurring": skipped_recurring,
                "skipped_oneshot": skipped_oneshot,
            })
        ),
        "Scheduler startup skip: advanced overdue jobs without executing",
    );
}

pub async fn execute_job_now(config: &Config, job: &CronJob) -> (bool, String) {
    // Reject orphaned declarative jobs: a declarative row whose canonical
    // config declaration has been removed must not execute through any
    // path (automatic polling or manual trigger).
    if job.source == "declarative" && !super::store::is_valid_declarative_owner(config, &job.id) {
        return (
            false,
            format!(
                "cron job {id:?} is an orphaned declarative entry \
                 (source = \"declarative\" but absent from live config); \
                 cannot execute",
                id = job.id
            ),
        );
    }
    use zeroclaw_log::Instrument;
    let Some(agent_alias) = resolve_owning_agent(config, job) else {
        return (
            false,
            format!(
                "cron job {id:?} has no owning agent; add the alias to an [agents.<x>].cron_jobs list",
                id = job.id
            ),
        );
    };
    let agent_alias = agent_alias.to_string();
    let security = match SecurityPolicy::for_agent(config, &agent_alias) {
        Ok(s) => s,
        Err(e) => return (false, format!("agent {agent_alias} risk profile: {e}")),
    };
    let span = zeroclaw_log::attribution_span!(job);
    Box::pin(execute_job_with_retry(config, &security, &agent_alias, job))
        .instrument(span)
        .await
}

fn cron_agent_run_security_policy(base: &SecurityPolicy, job: &CronJob) -> SecurityPolicy {
    let mut policy = base.clone();
    if !matches!(job.job_type, JobType::Agent) || job.allowed_tools.is_some() {
        return policy;
    }

    let excluded = policy.excluded_tools.get_or_insert_with(Vec::new);
    for tool in CRON_AGENT_DEFAULT_EXCLUDED_TOOLS {
        if !excluded.iter().any(|existing| existing == tool) {
            excluded.push((*tool).to_string());
        }
    }
    policy
}

fn cron_agent_session_path(target: &SessionTarget, run_session_id: &str) -> std::path::PathBuf {
    match target {
        SessionTarget::Main => std::path::PathBuf::from("main"),
        SessionTarget::Isolated => std::path::PathBuf::from(format!("cron-{run_session_id}")),
    }
}

async fn execute_job_with_retry(
    config: &Config,
    security: &SecurityPolicy,
    agent_alias: &str,
    job: &CronJob,
) -> (bool, String) {
    let mut last_output = String::new();
    let retries = config.reliability.scheduler_retries;
    let mut backoff_ms = config.reliability.provider_backoff_ms.max(200);

    for attempt in 0..=retries {
        let (success, output) = match job.job_type {
            JobType::Shell => run_job_command(config, security, job).await,
            JobType::Agent => Box::pin(run_agent_job(config, security, agent_alias, job)).await,
        };
        last_output = output;

        if success {
            return (true, last_output);
        }

        if last_output.starts_with("blocked by security policy:")
            || last_output.starts_with(AGENT_JOB_TIMEOUT_PREFIX)
        {
            // Deterministic policy violations and agent-run timeouts are not
            // retryable: a hung provider/tool will hang again, and retrying
            // would occupy the job's slot for `retries + 1` full timeouts.
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

fn claim_due_jobs(config: &Config, jobs: Vec<CronJob>) -> Vec<CronJob> {
    jobs.into_iter()
        .filter(|job| match claim_job(config, &job.id, Utc::now()) {
            Ok(true) => true,
            Ok(false) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"job_id": job.id})),
                    "Cron job already in flight; skipping duplicate launch"
                );
                false
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"job_id": job.id, "error": format!("{}", e)})
                        ),
                    "Cron job: failed to claim in-flight lock; skipping launch"
                );
                false
            }
        })
        .collect()
}

async fn process_due_jobs(
    config: &Config,
    jobs: Vec<CronJob>,
    component: &str,
    event_tx: &EventBroadcast,
) {
    // Refresh scheduler health on every successful poll cycle, including idle cycles.
    crate::health::mark_component_ok(component);

    let max_concurrent = config.scheduler.max_concurrent.max(1);
    let mut in_flight = stream::iter(jobs.into_iter().filter_map(|job| {
        let Some(agent_alias) = resolve_owning_agent(config, &job) else {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"job_id": job.id})), "Cron job has no owning agent; add the alias to an [agents.<x>].cron_jobs list");
            let _ = release_job(config, &job.id);
            return None;
        };
        let agent_alias = agent_alias.to_owned();
        let security = match SecurityPolicy::for_agent(config, &agent_alias) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"job_id": job.id, "agent": agent_alias, "error": format!("{}", e)})), "Cron job: failed to build SecurityPolicy for owning agent");
                let _ = release_job(config, &job.id);
                return None;
            }
        };
        let config = config.clone();
        let component = component.to_owned();
        Some(async move {
            Box::pin(execute_and_persist_job(
                &config,
                security.as_ref(),
                &agent_alias,
                &job,
                &component,
            ))
            .await
        })
    }))
    .buffer_unordered(max_concurrent);

    while let Some((job_id, success, output)) = in_flight.next().await {
        if !success {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"job_id": job_id, "output": output})),
                "Scheduler job '' failed: "
            );
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
    agent_alias: &str,
    job: &CronJob,
    component: &str,
) -> (String, bool, String) {
    crate::health::mark_component_ok(component);
    warn_if_high_frequency_agent_job(job);

    let started_at = Utc::now();
    let span = zeroclaw_log::attribution_span!(job);
    let (success, output) = Box::pin(execute_job_with_retry(config, security, agent_alias, job))
        .instrument(span)
        .await;
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

    // Release the in-flight lock claimed during selection (`claim_due_jobs`) now
    // that the run (and its reschedule/disable/delete in `persist_job_result`) is
    // done. A deleted one-shot row simply releases nothing. If this fails the lock
    // is recovered by `clear_stale_locks` at the next startup
    if let Err(e) = release_job(config, &job.id) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"job_id": job.id, "error": format!("{}", e)})),
            "Cron job: failed to release in-flight lock after run"
        );
    }

    (job.id.clone(), success, output)
}

/// Resolve the wall-clock deadline for an agent cron job's `agent::run`
/// call: the owning agent's runtime profile `agentic_timeout_secs` override,
/// falling back to `AGENT_JOB_TIMEOUT_SECS`.
fn resolve_agent_job_timeout(config: &Config, agent_alias: &str) -> Duration {
    Duration::from_secs(
        config
            .runtime_profile_for_agent(agent_alias)
            .and_then(|p| p.agentic_timeout_secs)
            // Config does not validate this per-profile field; a 0 override
            // would time every job out instantly and non-retryably, so treat
            // it as unset and fall back to the default.
            .filter(|&secs| secs > 0)
            .unwrap_or(AGENT_JOB_TIMEOUT_SECS),
    )
}

// Test-only seam: lets a scheduler test substitute the isolated-session
// purge's `Memory` handle with a deliberately-stalling double, so
// `ISOLATED_SESSION_PURGE_TIMEOUT` can be exercised deterministically
// without depending on a real backend's timing. Unset in production and in
// every test that doesn't explicitly scope it; `try_with` then fails closed
// to the real `create_memory_for_agent` construction below.
#[cfg(test)]
tokio::task_local! {
    static TEST_PURGE_MEMORY: Arc<dyn zeroclaw_api::memory_traits::Memory>;
}

/// Best-effort purge of an Isolated cron run's per-run memory session. The
/// Isolated session path (`cron-{run_session_id}`) is unique per run, so a
/// timed-out run whose dropped agent future may still have an in-flight
/// blocking sqlite write races only its own abandoned rows; a no-op for
/// Main-target runs (they share the stable `main` session).
///
/// This is called from `run_agent_job_with_timeout`'s timeout and
/// run-error branches, both of which are still awaited by
/// `execute_and_persist_job` before it persists the result and calls
/// `release_job`. A stalled backend (network stall, subprocess hang, lock
/// contention) must never delay that critical path indefinitely, so the
/// whole purge attempt (memory construction plus the backend call) runs
/// under `ISOLATED_SESSION_PURGE_TIMEOUT`. On timeout the cleanup is
/// abandoned (logged at WARN) rather than awaited further; the successful,
/// fast-cleanup path is unaffected.
async fn purge_isolated_session(
    config: &Config,
    job: &CronJob,
    agent_alias: &str,
    session_path: &std::path::Path,
) {
    if !matches!(job.session_target, SessionTarget::Isolated) {
        return;
    }
    let mem_session_key = zeroclaw_api::session_keys::sanitize_session_key(&format!(
        "cli:{}",
        session_path.display()
    ));

    // Owned copies: the construction below runs on a spawned task, which must
    // be `'static` and so cannot borrow `config` / `agent_alias`.
    let owned_config = config.clone();
    let owned_alias = agent_alias.to_string();
    let owned_api_key = config
        .model_provider_for_agent(agent_alias)
        .and_then(|e| e.api_key.as_deref().map(str::to_string));

    // Read the task-local here, on the caller's task: task-locals are NOT
    // inherited by spawned tasks, so this must be captured before any spawn.
    #[cfg(test)]
    let test_purge_memory = TEST_PURGE_MEMORY.try_with(Arc::clone).ok();

    let purge = async move {
        #[cfg(test)]
        if let Some(mem) = test_purge_memory {
            let _ = mem.purge_session(&mem_session_key).await;
            return;
        }

        // Construct the backend on its own task for the same reason the agent
        // run is spawned above: `create_memory_for_agent` can block inside a
        // single poll (PostgreSQL connect/migrate joins a thread), which would
        // otherwise starve this cleanup deadline and delay lock release.
        let construct = ::zeroclaw_spawn::spawn!(async move {
            zeroclaw_memory::create_memory_for_agent(
                &owned_config,
                &owned_alias,
                owned_api_key.as_deref(),
            )
            .await
        });
        let abort = construct.abort_handle();
        match construct.await {
            Ok(Ok(mem)) => {
                let _ = mem.purge_session(&mem_session_key).await;
            }
            // Construction failed or the task died; cleanup is best-effort.
            Ok(Err(_)) | Err(_) => abort.abort(),
        }
    };

    if time::timeout(ISOLATED_SESSION_PURGE_TIMEOUT, purge)
        .await
        .is_err()
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "job_id": job.id,
                    "agent": agent_alias,
                    "timeout_secs": ISOLATED_SESSION_PURGE_TIMEOUT.as_secs(),
                })),
            "Cron job: isolated-session purge exceeded its cleanup deadline; abandoning \
             best-effort cleanup so lock release is not delayed"
        );
    }
}

async fn run_agent_job(
    config: &Config,
    security: &SecurityPolicy,
    agent_alias: &str,
    job: &CronJob,
) -> (bool, String) {
    let timeout = resolve_agent_job_timeout(config, agent_alias);
    run_agent_job_with_timeout(config, security, agent_alias, job, timeout).await
}

async fn run_agent_job_with_timeout(
    config: &Config,
    security: &SecurityPolicy,
    agent_alias: &str,
    job: &CronJob,
    timeout: Duration,
) -> (bool, String) {
    let subagent_ctx = match crate::subagent::SubAgentSpawn::for_agent(config, agent_alias)
        .and_then(|spawn| spawn.build(crate::subagent::SubAgentOverrides::default()))
    {
        Ok(ctx) => ctx,
        Err(e) => return (false, format!("subagent spawn failed: {e:#}")),
    };

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

    let prefixed_prompt = format!("[cron:{} {name}] {prompt}", job.id);
    let model_override = job.model.clone();

    let mut cron_config = config.clone();
    cron_config.memory.auto_save = false;

    // Assign a unique run ID for tracing. Isolated jobs also use it in the
    // session path so failed-run memory purge stays scoped per execution.
    // Main-target jobs reuse the stable `main` session path documented in
    // `session_target`.
    let run_session_id = uuid::Uuid::new_v4().to_string();
    let session_path = cron_agent_session_path(&job.session_target, &run_session_id);

    let subagent_span = zeroclaw_log::info_span!(
        "subagent",
        category = "cron",
        agent_alias = %agent_alias,
        cron_job_id = %job.id,
        run_id = %run_session_id,
        spawn_site = "cron",
    );

    let run_security = cron_agent_run_security_policy(subagent_ctx.policy.as_ref(), job);
    let run_overrides = crate::agent::loop_::AgentRunOverrides {
        security: Some(Arc::new(run_security)),
        memory: None,
        is_subagent: false,
        // `uses_memory = false` fully opts the job out of the engine's
        // memory-context injection (stateless digest jobs)...
        suppress_memory_inject: !job.uses_memory,
        // ...and makes the run memory-free end to end: the loop binds a
        // `NoneMemory` backend and drops the persistent memory tools, so a
        // `uses_memory = false` job can neither recall/store through a real
        // backend nor reach one via advertised memory tools
        memory_free: !job.uses_memory,
        // Cron runs are short-lived and one-shot — no cross-turn reuse
        // contract, so the per-call `connect_all` path inside
        // `agent::run` is the correct choice. The daemon heartbeat
        // worker is the only `mcp_registry` supplier.
        mcp_registry: None,
    };
    let run_result = match job.session_target {
        SessionTarget::Main | SessionTarget::Isolated => {
            // Run the agent on its OWN task rather than inline under
            // `time::timeout`. A timer can only fire when the future holding it
            // yields, and `agent::run` performs synchronous setup before its
            // first await point: the PostgreSQL memory backend spawns an
            // initializer thread and immediately `join()`s it while that thread
            // connects, initializes schema, and runs migrations (see
            // `PostgresMemory::initialize_client`). Inline, that work happens
            // inside a single poll, so neither the timeout arm nor the
            // subsequent `release_job` could run and the advertised wall-clock
            // deadline would silently depend on the agent future cooperating.
            //
            // Spawning moves that blocking stretch onto a worker thread, and
            // leaves this task parked on `timeout` + `JoinHandle` — a boundary
            // whose timer stays schedulable no matter what the run does. On
            // timeout we abort the handle so the abandoned run cannot keep
            // using the runtime after the deadline.
            // Read outside the spawn: task-locals are not inherited by
            // spawned tasks.
            #[cfg(test)]
            let pre_run_block = TEST_PRE_RUN_BLOCK.try_with(|d| *d).ok();
            let run_alias = agent_alias.to_string();
            let run_temperature = config
                .model_provider_for_agent(agent_alias)
                .and_then(|e| e.temperature);
            let run_session_path = session_path.clone();
            let run_allowed_tools = job.allowed_tools.clone();
            let handle = ::zeroclaw_spawn::spawn!(
                async move {
                    #[cfg(test)]
                    if let Some(d) = pre_run_block {
                        std::thread::sleep(d);
                    }
                    Box::pin(crate::agent::run(
                        cron_config,
                        &run_alias,
                        Some(prefixed_prompt),
                        None,
                        model_override,
                        run_temperature,
                        vec![],
                        false,
                        Some(run_session_path),
                        run_allowed_tools,
                        zeroclaw_api::ingress::TurnOrigin::Cron,
                        run_overrides,
                    ))
                    .await
                }
                .instrument(subagent_span)
            );
            let abort = handle.abort_handle();

            match time::timeout(timeout, handle).await {
                // The run task completed on its own; unwrap the join layer.
                Ok(Ok(run_result)) => run_result,
                // The run task panicked or was aborted. Surface it as a
                // non-timeout failure so the retry classifier treats it like
                // any other run error rather than as a deadline breach.
                Ok(Err(join_err)) => Err(anyhow::Error::msg(format!(
                    "agent run task failed to complete: {join_err}"
                ))),
                Err(_) => {
                    abort.abort();
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"job_id": job.id, "timeout_secs": timeout.as_secs()})),
                        "Cron job: agent run timed out"
                    );
                    purge_isolated_session(config, job, agent_alias, &session_path).await;
                    return (
                        false,
                        format!("{AGENT_JOB_TIMEOUT_PREFIX}{}s", timeout.as_secs()),
                    );
                }
            }
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
            purge_isolated_session(config, job, agent_alias, &session_path).await;
            (false, format!("agent job failed: {e}"))
        }
    }
}

async fn persist_job_result(
    config: &Config,
    job: &CronJob,
    success: bool,
    output: &str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) -> bool {
    let duration_ms = (finished_at - started_at).num_milliseconds();
    let outcome = deliver_and_classify_run_result(
        config,
        job,
        success,
        output.to_string(),
        CronDeliveryContext::Scheduled,
    )
    .await;

    let action = if is_one_shot_auto_delete(job) && outcome.success {
        RunCompletionAction::Delete
    } else if matches!(job.schedule, Schedule::At { .. }) {
        RunCompletionAction::Disable
    } else {
        RunCompletionAction::Reschedule
    };

    let job_state_at = Utc::now();
    if let Err(e) = persist_run_result(
        config,
        job,
        started_at,
        finished_at,
        job_state_at,
        &outcome.status,
        Some(&outcome.output),
        duration_ms,
        action,
    ) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"e": e.to_string()})),
            "Failed to persist scheduler run result: "
        );

        if action == RunCompletionAction::Delete {
            // Best-effort fallback for the legacy behavior: a successful
            // auto-delete one-shot should not be picked up again if the
            // combined history+state transaction fails while inserting or
            // pruning the run row.
            if let Err(disable_err) = persist_run_completion_state(
                config,
                job,
                job_state_at,
                &outcome.status,
                Some(&outcome.output),
                RunCompletionAction::Disable,
            ) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"disable_err": disable_err.to_string()})),
                    "Failed to disable one-shot cron job after history persistence failure: "
                );
            }
        } else {
            // For recurring jobs and non-delete one-shots, keep the scheduler
            // moving even if run-history persistence fails.
            if let Err(state_err) = persist_run_completion_state(
                config,
                job,
                job_state_at,
                &outcome.status,
                Some(&outcome.output),
                action,
            ) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"state_err": state_err.to_string()})),
                    "Failed to update cron job state after history persistence failure: "
                );
            }
        }
    }

    outcome.success
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
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "Cron agent job '{}' is scheduled more frequently than every 5 minutes",
                job.id
            )
        );
    }
}

async fn deliver_if_configured(config: &Config, job: &CronJob, output: &str) -> Result<()> {
    let delivery: &DeliveryConfig = &job.delivery;
    if !delivery.mode.eq_ignore_ascii_case("announce") {
        return Ok(());
    }

    if !announce_delivery_decision(output).should_deliver() {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({"job_id": job.id})),
            "Cron job returned NO_REPLY sentinel — skipping delivery"
        );
        return Ok(());
    }

    let channel = delivery.channel.as_deref().ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"field": "channel"})),
            "cron delivery announce refused: required field missing"
        );
        anyhow::Error::msg("delivery.channel is required for announce mode")
    })?;
    let target = delivery.to.as_deref().ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"field": "to"})),
            "cron delivery announce refused: required field missing"
        );
        anyhow::Error::msg("delivery.to is required for announce mode")
    })?;

    deliver_announcement(
        config,
        channel,
        target,
        delivery.thread_id.as_deref(),
        output,
    )
    .await
}

/// Delivery function type — takes owned values so the returned future is 'static.
/// The fourth `Option<String>` is the optional thread/conversation id propagated
/// to channels whose outbound `thread_id` is distinct from the recipient (webhook).
pub type DeliveryFn = Box<
    dyn Fn(
            Config,
            String,
            String,
            Option<String>,
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
    thread_id: Option<&str>,
    output: &str,
) -> Result<()> {
    if let Some(f) = DELIVERY_FN.get() {
        f(
            config.clone(),
            channel.to_string(),
            target.to_string(),
            thread_id.map(str::to_string),
            output.to_string(),
        )
        .await
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"channel": channel, "target": target})),
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

    // `job.shell_output_format` is already the canonical value by the time
    // it reaches here: due_jobs()/all_overdue_jobs() resolve declarative jobs
    // from config and leave imperative jobs on their stored field (see
    // resolve_declarative_shell_output_format in store.rs). Re-deriving it
    // here from `config.cron.get(&job.id)` without checking `job.source`
    // would let an unrelated same-ID declarative config entry silently
    // override an imperative job's stored format.
    let output_format = &job.shell_output_format;

    let child = match build_cron_shell_command(&job.command, &config.data_dir) {
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
            let combined = match output_format {
                // Raw mode on success returns bare stdout, by design — the
                // point is to hand back exactly what a direct shell run
                // would print on stdout, with no wrapper. stderr on a
                // successful exit is intentionally dropped, not lost by
                // accident; a failing exit still gets the full wrapped
                // status/stdout/stderr envelope below for diagnosis.
                CronShellOutputFormat::Raw if output.status.success() => stdout.trim().to_string(),
                _ => format!(
                    "status={}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    stdout.trim(),
                    stderr.trim()
                ),
            };
            (output.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("spawn error: {e}")),
        Err(_) => (
            false,
            format!("job timed out after {}s", timeout.as_secs_f64()),
        ),
    }
}

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

    const TEST_AGENT: &str = "test-agent";

    #[test]
    fn is_no_reply_sentinel_matches_bare_form_case_insensitively() {
        assert!(is_no_reply_sentinel("NO_REPLY"));
        assert!(is_no_reply_sentinel("no_reply"));
        assert!(is_no_reply_sentinel("No_Reply"));
        // Trim tolerance.
        assert!(is_no_reply_sentinel("  NO_REPLY  "));
        assert!(is_no_reply_sentinel("\nNO_REPLY\n"));
    }

    #[test]
    fn is_no_reply_sentinel_matches_quiet_info_and_legacy_prefixes() {
        // Legacy form is documented as "treated as INFO".
        assert!(is_no_reply_sentinel("NO_REPLY: nothing to report"));
        assert!(is_no_reply_sentinel("  NO_REPLY: trimmed  "));
        // Explicit informational kind.
        assert!(is_no_reply_sentinel("NO_REPLY[INFO]: all healthy"));
        assert!(is_no_reply_sentinel("no_reply[info]: all healthy"));
        // Bracket whitespace tolerance.
        assert!(is_no_reply_sentinel("NO_REPLY[ info ]: spaced"));
    }

    #[test]
    fn is_no_reply_sentinel_does_not_suppress_failure_or_refusal_kinds() {
        // REFUSE / FAIL carry operator-visible meaning. In the cron/heartbeat
        // announce context there is no reaction side-channel, so suppressing
        // them would silently drop a failure/refusal the operator must see
        // review feedback).
        assert!(!is_no_reply_sentinel(
            "NO_REPLY[FAIL]: database check timed out"
        ));
        assert!(!is_no_reply_sentinel("no_reply[fail]: timed out"));
        assert!(!is_no_reply_sentinel(
            "NO_REPLY[REFUSE]: policy prevented the check"
        ));
        assert!(!is_no_reply_sentinel("no_reply[refuse]: blocked"));
        // Unknown/future kinds are conservatively delivered, not suppressed.
        assert!(!is_no_reply_sentinel("NO_REPLY[WARN]: disk at 90%"));
        // Malformed kinded form with no closing bracket is delivered.
        assert!(!is_no_reply_sentinel("NO_REPLY[INFO without close"));
    }

    #[test]
    fn is_no_reply_sentinel_rejects_real_content() {
        assert!(!is_no_reply_sentinel(""));
        assert!(!is_no_reply_sentinel("   "));
        assert!(!is_no_reply_sentinel("All systems nominal"));
        // Sentinel-looking but not a sentinel: word embedded in real prose.
        assert!(!is_no_reply_sentinel(
            "The job returned NO_REPLY which means nothing happened"
        ));
        assert!(!is_no_reply_sentinel("NO_REPLYING is the status"));
    }

    async fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.runtime_profiles.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig::default(),
        );
        config.providers.models.openrouter.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::OpenRouterModelProviderConfig::default(),
        );
        config.agents.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: format!("openrouter.{TEST_AGENT}").into(),
                risk_profile: TEST_AGENT.into(),
                runtime_profile: TEST_AGENT.into(),
                ..Default::default()
            },
        );
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        config
    }

    fn test_security(config: &Config) -> SecurityPolicy {
        SecurityPolicy::for_agent(config, TEST_AGENT).expect("test-agent has resolvable profiles")
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
            agent_alias: TEST_AGENT.into(),
            enabled: true,
            delivery: DeliveryConfig::default(),
            delete_after_run: false,
            allowed_tools: None,
            uses_memory: true,
            source: "imperative".into(),
            shell_output_format: CronShellOutputFormat::default(),
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

    #[test]
    fn cron_agent_session_path_main_is_stable() {
        assert_eq!(
            cron_agent_session_path(&SessionTarget::Main, "ignored"),
            std::path::PathBuf::from("main")
        );
        assert_eq!(
            cron_agent_session_path(&SessionTarget::Isolated, "abc").to_string_lossy(),
            "cron-abc"
        );
    }

    #[test]
    fn cron_agent_run_security_policy_excludes_scheduler_mutation_tools_by_default() {
        let security = SecurityPolicy::default();
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.allowed_tools = None;

        let policy = cron_agent_run_security_policy(&security, &job);

        for tool in [
            "cron_add",
            "cron_update",
            "cron_remove",
            "cron_run",
            "schedule",
        ] {
            assert!(
                !policy.is_tool_allowed(tool),
                "{tool} must be excluded from default cron agent runs"
            );
        }
        assert!(
            policy.is_tool_allowed("http_request"),
            "non-scheduler tools remain available when the base policy is unrestricted"
        );
    }

    #[test]
    fn cron_agent_run_security_policy_respects_explicit_allowed_tools() {
        let security = SecurityPolicy::default();
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.allowed_tools = Some(vec!["cron_add".into()]);

        let policy = cron_agent_run_security_policy(&security, &job);

        assert!(
            policy.is_tool_allowed("cron_add"),
            "explicit cron job allowed_tools should remain the override for intentional scheduler automation"
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo scheduler-ok");
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(success);
        assert!(output.contains("scheduler-ok"));
        assert!(output.contains("status=exit status: 0"));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_raw_output_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        // The store layer resolves shell_output_format before handing the job
        // to the scheduler (see resolve_declarative_shell_output_format), so
        // the job's own field is already canonical by the time it gets here.
        let mut job = test_job("echo raw-format-ok");
        job.shell_output_format = CronShellOutputFormat::Raw;
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(success);
        // Raw output should be just the command's trimmed stdout, no wrapper.
        assert_eq!(output, "raw-format-ok");
        assert!(!output.contains("status="));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_raw_output_success_drops_stderr() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        // A zero-exit command that still writes to stderr (e.g. a tool's
        // progress/warning chatter) must not leak into raw-mode output.
        let mut job = test_job("echo raw-stdout-ok; echo raw-stderr-noise >&2");
        job.shell_output_format = CronShellOutputFormat::Raw;
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(success);
        // Dropping stderr on a successful exit is intentional design, not
        // an oversight — see the comment at the call site.
        assert_eq!(output, "raw-stdout-ok");
        assert!(!output.contains("raw-stderr-noise"));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_raw_output_failure_still_uses_wrapped() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("ls definitely_missing_file_raw_test");
        job.shell_output_format = CronShellOutputFormat::Raw;
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        // On failure, raw mode should still include the wrapped format
        // so operators can diagnose the failure.
        assert!(output.contains("status=exit status:"));
        assert!(output.contains("definitely_missing_file_raw_test"));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_imperative_job_ignores_same_id_declarative_config_entry() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        // An unrelated declarative config entry happens to share the
        // imperative job's ID and asks for raw output. Execution must go by
        // the job's own (already-resolved) field, not re-derive from config
        // by ID match, or the imperative job's stored format gets silently
        // overridden.
        config.cron.insert(
            "test-job".into(),
            zeroclaw_config::schema::CronJobDecl {
                command: Some("echo collision-ok".into()),
                shell_output_format: CronShellOutputFormat::Raw,
                ..Default::default()
            },
        );
        let mut job = test_job("echo collision-ok");
        job.source = "imperative".into();
        job.shell_output_format = CronShellOutputFormat::Wrapped;
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(success);
        assert!(
            output.contains("status="),
            "imperative job's own Wrapped format must win over a same-ID declarative config entry: {output}"
        );
    }

    #[tokio::test]
    async fn run_manual_job_persists_history_and_broadcasts() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        let job = cron::add_shell_job_with_approval(
            &config,
            TEST_AGENT,
            Some("manual-run".into()),
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo rpc-manual-ok",
            None,
            true,
        )
        .expect("test job should be persisted");
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let event_tx = Some(tx);

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &event_tx).await;

        assert!(result.success);
        assert_eq!(result.status, "ok");
        assert!(result.output.contains("rpc-manual-ok"));

        let updated = cron::get_job(&config, &job.id).expect("job state should update");
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
        assert!(
            updated
                .last_output
                .as_deref()
                .is_some_and(|output| output.contains("rpc-manual-ok"))
        );

        let runs = cron::list_runs(&config, &job.id, 10).expect("run history should list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
        assert!(
            runs[0]
                .output
                .as_deref()
                .unwrap_or("")
                .contains("rpc-manual-ok")
        );

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("manual trigger should broadcast")
            .expect("broadcast channel should stay open");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["job_id"], job.id);
        assert_eq!(event["success"], true);
        assert_eq!(event["manual"], true);
        assert!(
            event["output"]
                .as_str()
                .unwrap_or("")
                .contains("rpc-manual-ok")
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_scheduler_test");
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("definitely_missing_file_for_scheduler_test"));
        assert!(output.contains("status=exit status:"));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_times_out() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["sleep".into()];
        let job = test_job("sleep 1");
        let security = test_security(&config);

        let (success, output) =
            run_job_command_with_timeout(&config, &security, &job, Duration::from_millis(50)).await;
        assert!(!success);
        assert!(output.contains("job timed out after"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_disallowed_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        let job = test_job("curl https://evil.example");
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.to_lowercase().contains("not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["cat".into()];
        let outside_path = absolute_path_outside_workspace();
        let job = test_job(&format!("cat {outside_path}"));
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains(outside_path));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_option_assignment_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["grep".into()];
        let outside_path = absolute_path_outside_workspace();
        let job = test_job(&format!("grep --file={outside_path} root ./src"));
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains(outside_path));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_short_option_attached_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["grep".into()];
        let outside_path = absolute_path_outside_workspace();
        let job = test_job(&format!("grep -f{outside_path} root ./src"));
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains(outside_path));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_blocks_tilde_user_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["cat".into()];
        let job = test_job("cat ~root/.ssh/id_rsa");
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("~root/.ssh/id_rsa"));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn run_job_command_blocks_input_redirection_path_bypass() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["cat".into()];
        let job = test_job("cat </etc/passwd");
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.to_lowercase().contains("not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = crate::security::AutonomyLevel::ReadOnly;
        let job = test_job("echo should-not-run");
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .runtime_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .max_actions_per_hour = 0;
        let job = test_job("echo should-not-run");
        let security = test_security(&config);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[cfg(target_os = "windows")]
    fn absolute_path_outside_workspace() -> &'static str {
        r"C:\Windows\win.ini"
    }

    #[cfg(not(target_os = "windows"))]
    fn absolute_path_outside_workspace() -> &'static str {
        "/etc/passwd"
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn execute_job_with_retry_recovers_after_first_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["sh".into()];
        let security = test_security(&config);

        tokio::fs::write(
            config.data_dir.join("retry-once.sh"),
            "#!/bin/sh\nif [ -f retry-ok.flag ]; then\n  echo recovered\n  exit 0\nfi\ntouch retry-ok.flag\nexit 1\n",
        )
        .await
        .unwrap();
        let job = test_job("sh ./retry-once.sh");

        let (success, output) = Box::pin(execute_job_with_retry(
            &config,
            &security,
            "test-agent",
            &job,
        ))
        .await;
        assert!(success);
        assert!(output.contains("recovered"));
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn execute_job_with_retry_exhausts_attempts() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        let security = test_security(&config);

        let job = test_job("ls always_missing_for_retry_test");

        let (success, output) = Box::pin(execute_job_with_retry(
            &config,
            &security,
            "test-agent",
            &job,
        ))
        .await;
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
        let security = test_security(&config);

        let (success, output) =
            Box::pin(run_agent_job(&config, &security, "test-agent", &job)).await;
        assert!(!success);
        assert!(output.contains("agent job failed:"));
    }

    #[tokio::test]
    async fn run_agent_job_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = crate::security::AutonomyLevel::ReadOnly;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = test_security(&config);

        let (success, output) =
            Box::pin(run_agent_job(&config, &security, "test-agent", &job)).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    /// Bind a loopback listener that accepts connections but never writes a
    /// response, simulating a provider whose HTTP request hangs forever.
    /// Each accepted connection is held open (not reset) well past the
    /// timeouts under test below, so the client observes a genuine hang
    /// rather than a fast connection-refused/reset error. The returned
    /// counter records how many connections were accepted, so a test can
    /// assert the exact number of provider HTTP attempts.
    async fn spawn_hanging_server() -> (
        std::net::SocketAddr,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepts_task = std::sync::Arc::clone(&accepts);
        ::zeroclaw_spawn::spawn!(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                accepts_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ::zeroclaw_spawn::spawn!(async move {
                    let _stream = stream;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        (addr, accepts)
    }

    /// `test_config` with `TEST_AGENT` repointed at a `custom` (openai-compatible)
    /// model_provider whose `uri` targets `addr`. `openrouter`'s family factory
    /// hardcodes its base URL and ignores the config `uri` override, so the
    /// `custom` family is used here specifically because it is the one slot
    /// that honors `uri` end to end — this is what lets a hung listener stand
    /// in for a hung provider HTTP call in `agent::run`.
    async fn test_config_with_hanging_provider(
        tmp: &TempDir,
        addr: std::net::SocketAddr,
    ) -> Config {
        let mut config = test_config(tmp).await;
        config.providers.models.custom.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::CustomModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    uri: Some(format!("http://{addr}")),
                    api_key: Some("test-key".to_string()),
                    model: Some("test-model".to_string()),
                    ..Default::default()
                },
            },
        );
        config.agents.get_mut(TEST_AGENT).unwrap().model_provider =
            format!("custom.{TEST_AGENT}").into();
        config
    }

    #[tokio::test]
    async fn run_agent_job_with_timeout_reports_timeout_for_hung_provider() {
        let tmp = TempDir::new().unwrap();
        let (addr, _accepts) = spawn_hanging_server().await;
        let config = test_config_with_hanging_provider(&tmp, addr).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = test_security(&config);

        // 750ms is generous enough to survive loaded mac/arm64 CI without
        // preempting a fast-failing run, while keeping the test short.
        let started = std::time::Instant::now();
        let (success, output) = Box::pin(run_agent_job_with_timeout(
            &config,
            &security,
            TEST_AGENT,
            &job,
            Duration::from_millis(750),
        ))
        .await;
        let elapsed = started.elapsed();

        assert!(!success);
        assert!(
            output.starts_with(AGENT_JOB_TIMEOUT_PREFIX),
            "unexpected output: {output}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "run_agent_job_with_timeout did not return promptly: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn execute_and_persist_job_releases_lock_after_agent_timeout() {
        let tmp = TempDir::new().unwrap();
        let (addr, _accepts) = spawn_hanging_server().await;
        let mut config = test_config_with_hanging_provider(&tmp, addr).await;
        config
            .runtime_profiles
            .get_mut(TEST_AGENT)
            .unwrap()
            .agentic_timeout_secs = Some(1);

        let job = cron::add_agent_job(
            &config,
            TEST_AGENT,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "Say hello",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap();

        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job should be claimable before the run"
        );

        let security = test_security(&config);
        let component = unique_component("agent-timeout-release");
        let (job_id, success, output) = Box::pin(execute_and_persist_job(
            &config, &security, TEST_AGENT, &job, &component,
        ))
        .await;

        assert_eq!(job_id, job.id);
        assert!(!success);
        assert!(
            output.starts_with(AGENT_JOB_TIMEOUT_PREFIX),
            "unexpected output: {output}"
        );

        // The lock claimed for this run must be released once the timed-out
        // run completes, so the job is selectable again on the next poll
        // instead of being wedged until process restart.
        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job lock must be released after an agent-run timeout"
        );
    }

    /// Stands in for a memory backend whose `purge_session` never returns
    /// (a wedged subprocess, a stalled network call, or lock contention).
    /// `purge_isolated_session` only ever calls `purge_session` on this
    /// double, so every other trait method is a trivial stub.
    #[derive(Default)]
    struct StallingPurgeMemory {
        purge_attempts: std::sync::atomic::AtomicUsize,
    }

    impl ::zeroclaw_api::attribution::Attributable for StallingPurgeMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }

        fn alias(&self) -> &str {
            "stalling-purge-memory"
        }
    }

    #[async_trait::async_trait]
    impl ::zeroclaw_api::memory_traits::Memory for StallingPurgeMemory {
        fn name(&self) -> &str {
            "stalling-purge-memory"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: ::zeroclaw_api::memory_traits::MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<::zeroclaw_api::memory_traits::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(
            &self,
            _key: &str,
        ) -> anyhow::Result<Option<::zeroclaw_api::memory_traits::MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&::zeroclaw_api::memory_traits::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<::zeroclaw_api::memory_traits::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: ::zeroclaw_api::memory_traits::MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<::zeroclaw_api::memory_traits::MemoryEntry>> {
            Ok(Vec::new())
        }

        /// Never resolves: the stalled backend that
        /// `ISOLATED_SESSION_PURGE_TIMEOUT` must bound.
        async fn purge_session(&self, _session_id: &str) -> anyhow::Result<usize> {
            self.purge_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            unreachable!("purge_session must never resolve in this test double");
        }
    }

    #[tokio::test]
    async fn execute_and_persist_job_releases_lock_when_isolated_purge_stalls() {
        // Same setup as `execute_and_persist_job_releases_lock_after_agent_timeout`
        // (hung provider -> real agent-run timeout -> `purge_isolated_session`),
        // except the isolated-session purge backend itself now stalls forever.
        // The local sqlite test backend used by the sibling test purges fast
        // and so cannot exercise this failure mode; this double proves the
        // lock-release path stays bounded even when cleanup does not.
        let tmp = TempDir::new().unwrap();
        let (addr, _accepts) = spawn_hanging_server().await;
        let mut config = test_config_with_hanging_provider(&tmp, addr).await;
        config
            .runtime_profiles
            .get_mut(TEST_AGENT)
            .unwrap()
            .agentic_timeout_secs = Some(1);

        let job = cron::add_agent_job(
            &config,
            TEST_AGENT,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "Say hello",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap();

        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job should be claimable before the run"
        );

        let security = test_security(&config);
        let component = unique_component("isolated-purge-stall-release");
        let stalling_memory: Arc<dyn ::zeroclaw_api::memory_traits::Memory> =
            Arc::new(StallingPurgeMemory::default());

        let started = std::time::Instant::now();
        // Also bound the call from outside `execute_and_persist_job`: if
        // the fix regressed back to an unbounded await on the stalled
        // purge, this test must fail promptly rather than hang the suite.
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            TEST_PURGE_MEMORY.scope(
                stalling_memory,
                Box::pin(execute_and_persist_job(
                    &config, &security, TEST_AGENT, &job, &component,
                )),
            ),
        )
        .await;
        let elapsed = started.elapsed();

        let (job_id, success, output) = outcome.unwrap_or_else(|_| {
            panic!(
                "execute_and_persist_job did not return within the outer test bound \
                 ({elapsed:?}); a stalled isolated-session purge must not block \
                 persist/release_job"
            )
        });

        assert_eq!(job_id, job.id);
        assert!(!success);
        assert!(
            output.starts_with(AGENT_JOB_TIMEOUT_PREFIX),
            "unexpected output: {output}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "execute_and_persist_job did not return promptly despite a stalled \
             isolated-session purge: {elapsed:?}"
        );

        // The lock must be released even though the isolated-session purge
        // backend never returns: cleanup is bounded by
        // `ISOLATED_SESSION_PURGE_TIMEOUT` and abandoned rather than awaited
        // indefinitely, so `execute_and_persist_job` still reaches
        // `persist_job_result`/`release_job`.
        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job lock must be released even when isolated-session cleanup stalls"
        );
    }

    // Multi-threaded on purpose: production runs the scheduler under
    // `#[tokio::main]` (multi-thread). A run task that blocks its worker
    // thread can only be outlived by a timer if some other thread can still
    // drive it, which is exactly the property this test asserts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_and_persist_job_releases_lock_when_run_blocks_before_first_await() {
        // Blocker: `tokio::time::timeout` observes its deadline only when the
        // wrapped future yields. `agent::run` performs synchronous setup before
        // its first await point (PostgreSQL memory construction joins an
        // initializer thread that connects, migrates, and initializes schema).
        // With the run inlined under `timeout`, that work happens inside a
        // single poll, so neither the timeout arm nor `release_job` can run and
        // the advertised wall-clock deadline silently depends on the agent
        // future cooperating. The existing timeout tests all use a hanging
        // *provider*, which stalls at an await and therefore cannot catch this.
        let tmp = TempDir::new().unwrap();
        let (addr, _accepts) = spawn_hanging_server().await;
        let mut config = test_config_with_hanging_provider(&tmp, addr).await;
        config
            .runtime_profiles
            .get_mut(TEST_AGENT)
            .unwrap()
            .agentic_timeout_secs = Some(1);

        let job = cron::add_agent_job(
            &config,
            TEST_AGENT,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "Say hello",
            SessionTarget::Main,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap();

        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job should be claimable before the run"
        );

        let security = test_security(&config);
        let component = unique_component("pre-await-block-release");

        let started = std::time::Instant::now();
        // The blocking stretch (8s) deliberately outlasts the job's 1s
        // deadline by a wide margin, so the pass/fail split is unambiguous on
        // loaded CI: the fix returns in ~1s, the pre-fix shape in ~8s.
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            TEST_PRE_RUN_BLOCK.scope(
                Duration::from_secs(8),
                Box::pin(execute_and_persist_job(
                    &config, &security, TEST_AGENT, &job, &component,
                )),
            ),
        )
        .await;
        let elapsed = started.elapsed();

        let (job_id, success, output) = outcome.unwrap_or_else(|_| {
            panic!(
                "execute_and_persist_job did not return within the outer test bound \
                 ({elapsed:?}); synchronous pre-await setup must not monopolize the \
                 scheduler task past the configured deadline"
            )
        });

        assert_eq!(job_id, job.id);
        assert!(!success);
        assert!(
            output.starts_with(AGENT_JOB_TIMEOUT_PREFIX),
            "a run whose setup blocks past the deadline must be reported as a \
             timeout, got: {output}"
        );

        // The claim must be reclaimable within a short bound of the configured
        // 1s deadline -- NOT merely after the 4s block finishes, which is what
        // an inline `timeout` would produce.
        // Generous vs the 1s deadline (tolerates slow CI) but far below the
        // 8s block, so this can only pass if the deadline fired independently
        // of the blocking setup.
        assert!(
            elapsed < Duration::from_secs(5),
            "job was only released after the blocking setup completed ({elapsed:?}); \
             the deadline must not depend on the agent future yielding"
        );
        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job lock must be released even when run setup blocks past the deadline"
        );
    }

    #[tokio::test]
    async fn execute_and_persist_job_releases_lock_when_delivery_stalls() {
        // Blocker: `persist_job_result` awaits announcement delivery BEFORE
        // `execute_and_persist_job` calls `release_job`. A `DeliveryFn` that
        // never resolves (dead socket, wedged provider) would therefore leave
        // `locked_at` set forever even though the agent run and its purge both
        // returned under their own deadlines. Starts from a real agent timeout
        // because timeout output is operator-visible rather than a quiet
        // `NO_REPLY`, so an announce-mode job reliably reaches delivery.
        register_recording_delivery_fn();
        let tmp = TempDir::new().unwrap();
        let (addr, _accepts) = spawn_hanging_server().await;
        let mut config = test_config_with_hanging_provider(&tmp, addr).await;
        config
            .runtime_profiles
            .get_mut(TEST_AGENT)
            .unwrap()
            .agentic_timeout_secs = Some(1);

        let job = cron::add_agent_job(
            &config,
            TEST_AGENT,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "Say hello",
            SessionTarget::Main,
            None,
            Some(DeliveryConfig {
                mode: "announce".to_string(),
                channel: Some(STALL_CHANNEL.to_string()),
                to: Some("chat-id".to_string()),
                thread_id: None,
                // Not best-effort: proves the bounded path still classifies a
                // stalled send as a real delivery failure rather than silently
                // reporting success.
                best_effort: false,
            }),
            false,
            None,
            true,
        )
        .unwrap();

        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job should be claimable before the run"
        );

        let security = test_security(&config);
        let component = unique_component("delivery-stall-release");
        let before = STALL_DELIVERY_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst);

        let started = std::time::Instant::now();
        // Outer bound so a regression to an unbounded delivery await fails this
        // test promptly instead of hanging the suite.
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            TEST_DELIVERY_TIMEOUT.scope(
                Duration::from_millis(250),
                Box::pin(execute_and_persist_job(
                    &config, &security, TEST_AGENT, &job, &component,
                )),
            ),
        )
        .await;
        let elapsed = started.elapsed();

        let (job_id, success, _output) = outcome.unwrap_or_else(|_| {
            panic!(
                "execute_and_persist_job did not return within the outer test bound \
                 ({elapsed:?}); a stalled delivery must not block release_job"
            )
        });

        assert_eq!(job_id, job.id);
        assert!(
            !success,
            "a non-best-effort delivery that never completes must classify the run as failed"
        );
        assert!(
            STALL_DELIVERY_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst) > before,
            "delivery was never attempted; this test would vacuously pass"
        );

        // The point of the fix: the claim is released despite delivery never
        // completing.
        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "job lock must be released even when announcement delivery stalls"
        );
    }

    #[tokio::test]
    async fn execute_job_with_retry_does_not_retry_agent_timeout() {
        let tmp = TempDir::new().unwrap();
        let (addr, accepts) = spawn_hanging_server().await;
        let mut config = test_config_with_hanging_provider(&tmp, addr).await;
        config
            .runtime_profiles
            .get_mut(TEST_AGENT)
            .unwrap()
            .agentic_timeout_secs = Some(1);
        // Retries would otherwise occupy the job's slot for `retries + 1`
        // full timeouts; a non-retryable timeout must attempt exactly once.
        config.reliability.scheduler_retries = 2;
        config.reliability.provider_backoff_ms = 1;

        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = test_security(&config);

        let (success, output) =
            Box::pin(execute_job_with_retry(&config, &security, TEST_AGENT, &job)).await;

        assert!(!success);
        assert!(
            output.starts_with(AGENT_JOB_TIMEOUT_PREFIX),
            "unexpected output: {output}"
        );
        // Exactly one provider HTTP attempt: `scheduler_retries = 2` would
        // drive three connections if the timeout were retried.
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "agent timeout must not be retried"
        );
    }

    #[test]
    fn resolve_agent_job_timeout_honors_runtime_profile_override() {
        let mut config = Config::default();
        config.runtime_profiles.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig {
                agentic_timeout_secs: Some(7),
                ..Default::default()
            },
        );
        config.agents.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                runtime_profile: TEST_AGENT.into(),
                ..Default::default()
            },
        );

        assert_eq!(
            resolve_agent_job_timeout(&config, TEST_AGENT),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn resolve_agent_job_timeout_falls_back_to_default_without_override() {
        let mut config = Config::default();
        config.runtime_profiles.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig::default(),
        );
        config.agents.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                runtime_profile: TEST_AGENT.into(),
                ..Default::default()
            },
        );

        assert_eq!(
            resolve_agent_job_timeout(&config, TEST_AGENT),
            Duration::from_secs(AGENT_JOB_TIMEOUT_SECS)
        );
    }

    #[test]
    fn resolve_agent_job_timeout_falls_back_to_default_with_zero_override() {
        // `agentic_timeout_secs = 0` is not validated away at config load, so
        // the resolver must treat it as unset rather than an immediate,
        // non-retryable timeout (an agent-run timeout is deliberately not
        // retried, unlike shell-job/delegate timeouts elsewhere in the
        // config surface, so `Some(0)` must never reach `time::timeout` as a
        // real zero-duration deadline).
        let mut config = Config::default();
        config.runtime_profiles.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig {
                agentic_timeout_secs: Some(0),
                ..Default::default()
            },
        );
        config.agents.insert(
            TEST_AGENT.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                runtime_profile: TEST_AGENT.into(),
                ..Default::default()
            },
        );

        assert_eq!(
            resolve_agent_job_timeout(&config, TEST_AGENT),
            Duration::from_secs(AGENT_JOB_TIMEOUT_SECS)
        );
    }

    #[tokio::test]
    async fn run_agent_job_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .runtime_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .max_actions_per_hour = 0;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = test_security(&config);

        let (success, output) =
            Box::pin(run_agent_job(&config, &security, "test-agent", &job)).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn process_due_jobs_marks_component_ok_even_when_idle() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let component = unique_component("scheduler-idle");

        crate::health::mark_component_error(&component, "pre-existing error");
        process_due_jobs(&config, Vec::new(), &component, &None).await;

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
        let component = unique_component("scheduler-fail");

        crate::health::mark_component_ok(&component);
        process_due_jobs(&config, vec![job], &component, &None).await;

        let snapshot = crate::health::snapshot_json();
        let entry = &snapshot["components"][component.as_str()];
        assert_eq!(entry["status"], "ok");
    }

    #[tokio::test]
    async fn persist_job_result_records_run_and_reschedules_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
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
    async fn persist_job_result_uses_one_write_connection_for_recurring_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        crate::cron::store::reset_write_connection_count_for_tests(&config);
        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;

        assert!(success);
        assert_eq!(
            crate::cron::store::write_connection_count_for_tests(&config),
            1
        );
    }

    #[tokio::test]
    async fn persist_job_result_prunes_run_history_and_updates_last_fields() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.scheduler.max_run_history = 2;
        let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let base = Utc::now();

        for idx in 0..3 {
            let started = base + ChronoDuration::seconds(idx);
            let finished = started + ChronoDuration::milliseconds(10);
            let output = format!("run-{idx}");

            let success = persist_job_result(&config, &job, true, &output, started, finished).await;
            assert!(success);
        }

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].output.as_deref(), Some("run-2"));
        assert_eq!(runs[1].output.as_deref(), Some("run-1"));

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
        assert_eq!(updated.last_output.as_deref(), Some("run-2"));
        assert!(updated.last_run.is_some());
    }

    #[tokio::test]
    async fn persist_job_result_rolls_back_run_history_when_job_state_update_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let original_next_run = job.next_run;
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let conn =
            rusqlite::Connection::open(config.data_dir.join("cron").join("jobs.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_cron_job_update
             BEFORE UPDATE ON cron_jobs
             BEGIN
                 SELECT RAISE(ABORT, 'blocked update');
             END;",
        )
        .unwrap();
        drop(conn);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;

        assert!(success);
        assert!(cron::list_runs(&config, &job.id, 10).unwrap().is_empty());

        let stored = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(stored.next_run, original_next_run);
        assert!(stored.last_run.is_none());
        assert!(stored.last_status.is_none());
        assert!(stored.last_output.is_none());
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            TEST_AGENT,
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            None,
            true,
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
            TEST_AGENT,
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            None,
            true,
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
    async fn persist_job_result_uses_one_write_connection_for_failed_one_shot_disable() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            "test-agent",
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            None,
            true,
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        crate::cron::store::reset_write_connection_count_for_tests(&config);
        let success = persist_job_result(&config, &job, false, "boom", started, finished).await;

        assert!(!success);
        assert_eq!(
            crate::cron::store::write_connection_count_for_tests(&config),
            1
        );
    }

    #[tokio::test]
    async fn persist_job_result_falls_back_to_state_update_when_history_prune_fails() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.scheduler.max_run_history = 1;
        let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let original_next_run = job.next_run;
        let seed_started = Utc::now() - ChronoDuration::minutes(20);
        let seed_finished = seed_started + ChronoDuration::milliseconds(10);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let conn =
            rusqlite::Connection::open(config.data_dir.join("cron").join("jobs.db")).unwrap();
        conn.execute(
            "INSERT INTO cron_runs (job_id, started_at, finished_at, status, output, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                job.id,
                seed_started.to_rfc3339(),
                seed_finished.to_rfc3339(),
                "seed",
                "seed",
                10,
            ],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_cron_run_prune
             BEFORE DELETE ON cron_runs
             BEGIN
                 SELECT RAISE(ABORT, 'blocked prune');
             END;",
        )
        .unwrap();
        drop(conn);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "seed");

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
        assert_eq!(updated.last_output.as_deref(), Some("ok"));
        assert!(updated.last_run.is_some());
        assert!(updated.next_run >= original_next_run);
    }

    #[tokio::test]
    async fn persist_job_result_falls_back_to_disable_when_auto_delete_history_insert_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell").unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let conn =
            rusqlite::Connection::open(config.data_dir.join("cron").join("jobs.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_cron_run_insert
             BEFORE INSERT ON cron_runs
             BEGIN
                 SELECT RAISE(ABORT, 'blocked insert');
             END;",
        )
        .unwrap();
        drop(conn);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
        assert_eq!(updated.last_output.as_deref(), Some("ok"));
        assert!(cron::list_runs(&config, &job.id, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell").unwrap();
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
        let job = cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell").unwrap();
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
            TEST_AGENT,
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
                thread_id: None,
                best_effort: false,
            }),
            false,
            None,
            true,
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
    async fn persist_job_result_delivery_failure_best_effort_marks_degraded() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        register_recording_delivery_fn();
        let mut job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        job.delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: Some("fail-delivery".into()),
            to: Some("123456".into()),
            thread_id: None,
            best_effort: true,
        };
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("degraded"));
        assert!(
            updated
                .last_output
                .as_deref()
                .unwrap_or_default()
                .contains("delivery failed:")
        );

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "degraded");
    }

    #[tokio::test]
    async fn delivery_failure_classification_preserves_empty_output_evidence() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        register_recording_delivery_fn();
        let mut job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        job.delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: Some("fail-delivery".into()),
            to: Some("123456".into()),
            thread_id: None,
            best_effort: true,
        };

        let outcome = deliver_and_classify_run_result(
            &config,
            &job,
            true,
            String::new(),
            CronDeliveryContext::Scheduled,
        )
        .await;

        assert!(outcome.success);
        assert_eq!(outcome.status, "degraded");
        assert!(outcome.output.starts_with("delivery failed:"));
    }

    #[tokio::test]
    async fn persist_job_result_at_schedule_without_delete_after_run_is_disabled() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job = cron::add_agent_job(
            &config,
            TEST_AGENT,
            Some("at-no-autodelete".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
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

    static DELIVERED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Channel name the recorder counts. Used only by the suppression test.
    const COUNT_CHANNEL: &str = "count-delivery";

    /// Channel name whose delivery future never resolves. Exercises
    /// `CRON_DELIVERY_TIMEOUT`: a send that never completes must not hold the
    /// scheduler's in-flight claim open.
    const STALL_CHANNEL: &str = "stall-delivery";

    /// Counts how many times the stalling delivery handler was entered, so a
    /// test can prove delivery was actually attempted (and not skipped by, say,
    /// the `NO_REPLY` suppression path) before the deadline fired.
    static STALL_DELIVERY_ATTEMPTS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn register_recording_delivery_fn() {
        // Idempotent: register_delivery_fn is a no-op once the OnceLock is set,
        // so repeated calls across tests are safe and the first writer wins. The
        // handler honours the `fail-delivery` failure contract used by the
        // delivery-classification tests so it composes regardless of order.
        register_delivery_fn(Box::new(|_config, channel, _target, _thread, _output| {
            Box::pin(async move {
                if channel == "fail-delivery" {
                    anyhow::bail!("synthetic delivery failure");
                }
                if channel == STALL_CHANNEL {
                    STALL_DELIVERY_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::future::pending::<()>().await;
                    unreachable!("stalling delivery must never resolve in this test double");
                }
                if channel == COUNT_CHANNEL {
                    DELIVERED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(())
            })
        }));
    }

    fn announce_job() -> CronJob {
        let mut job = test_job("echo ok");
        job.delivery = DeliveryConfig {
            mode: "announce".to_string(),
            channel: Some(COUNT_CHANNEL.to_string()),
            to: Some("chat-id".to_string()),
            thread_id: None,
            best_effort: true,
        };
        job
    }

    #[tokio::test]
    async fn deliver_if_configured_suppresses_no_reply_but_delivers_real_and_failure() {
        register_recording_delivery_fn();
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = announce_job();
        use std::sync::atomic::Ordering::SeqCst;

        // Quiet sentinel forms must NOT trigger delivery.
        for quiet in [
            "NO_REPLY",
            "NO_REPLY: nothing to report",
            "NO_REPLY[INFO]: healthy",
        ] {
            let before = DELIVERED.load(SeqCst);
            deliver_if_configured(&config, &job, quiet).await.unwrap();
            assert_eq!(
                DELIVERED.load(SeqCst),
                before,
                "quiet sentinel {quiet:?} must be suppressed (no delivery)"
            );
        }

        // Real content must be delivered.
        let before = DELIVERED.load(SeqCst);
        deliver_if_configured(&config, &job, "All systems nominal")
            .await
            .unwrap();
        assert_eq!(
            DELIVERED.load(SeqCst),
            before + 1,
            "real content must be delivered"
        );

        // Failure / refusal kinds must be delivered (operator-visible).
        for visible in [
            "NO_REPLY[FAIL]: database check timed out",
            "NO_REPLY[REFUSE]: policy prevented the check",
        ] {
            let before = DELIVERED.load(SeqCst);
            deliver_if_configured(&config, &job, visible).await.unwrap();
            assert_eq!(
                DELIVERED.load(SeqCst),
                before + 1,
                "failure/refusal kind {visible:?} must be delivered, not suppressed"
            );
        }
    }

    #[test]
    fn heartbeat_announce_decision_matches_worker_behavior() {
        // NO_REPLY heartbeat: suppressed.
        assert!(!announce_delivery_decision("NO_REPLY").should_deliver());
        assert!(!announce_delivery_decision("NO_REPLY[INFO]: all good").should_deliver());
        // Non-sentinel heartbeat output: delivered.
        assert!(announce_delivery_decision("disk usage 42%").should_deliver());
        // Empty-output fallback string the worker builds: must deliver.
        assert!(
            announce_delivery_decision("💓 heartbeat task completed: db health").should_deliver(),
            "the empty-output heartbeat fallback must never be mistaken for a sentinel"
        );
        // Failure/refusal kinds: delivered (operator-visible).
        assert!(announce_delivery_decision("NO_REPLY[FAIL]: db timed out").should_deliver());
        assert!(announce_delivery_decision("NO_REPLY[REFUSE]: blocked by policy").should_deliver());
    }

    #[tokio::test]
    async fn deliver_announcement_returns_ok_when_no_handler_registered() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        // No registered handler is a runtime-level state, not a delivery
        // failure. The caller (persist_job_result) should record the job
        // execution as successful; the missing handler is logged via
        // tracing::warn for operator visibility.
        deliver_announcement(&config, "telegram", "chat-id", None, "payload")
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
            let _ = cron::add_job(
                &config,
                "test-agent",
                "* * * * *",
                &format!("echo catchup-{i}"),
            )
            .unwrap();
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
        let mut config = test_config(&tmp).await;
        let job = test_job("echo broadcast-ok");
        // Bind the synthetic test job to test-agent so process_due_jobs's
        // owning-agent lookup succeeds (jobs without an owner are skipped).
        config
            .agents
            .get_mut("test-agent")
            .unwrap()
            .cron_jobs
            .push(job.id.clone());
        let component = unique_component("broadcast-ok");

        let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, vec![job], &component, &event_tx).await;

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
        let mut config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_broadcast_fail_test");
        config
            .agents
            .get_mut("test-agent")
            .unwrap()
            .cron_jobs
            .push(job.id.clone());
        let component = unique_component("broadcast-fail");

        let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, vec![job], &component, &event_tx).await;

        let event = rx.try_recv().expect("should receive a broadcast event");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["job_id"], "test-job");
        assert_eq!(event["success"], false);
        assert!(event["timestamp"].as_str().is_some());
    }

    #[tokio::test]
    async fn claim_due_jobs_skips_in_flight_job() {
        // once a due job is claimed for execution, a
        // subsequent selection pass must not pick it up again until the prior
        // run releases it — otherwise a job that runs longer than the poll
        // interval is launched repeatedly.
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();

        let claimed = claim_due_jobs(&config, vec![job.clone()]);
        assert_eq!(claimed.len(), 1, "first selection claims the job");

        let claimed_again = claim_due_jobs(&config, vec![job.clone()]);
        assert!(
            claimed_again.is_empty(),
            "an in-flight job must be skipped by the next selection pass"
        );

        cron::release_job(&config, &job.id).unwrap();
        let after_release = claim_due_jobs(&config, vec![job]);
        assert_eq!(
            after_release.len(),
            1,
            "after release the job is selectable again"
        );
    }

    #[tokio::test]
    async fn process_due_jobs_releases_lock_for_skipped_orphan_job() {
        // A job claimed for execution but then skipped by process_due_jobs (here
        // an orphan with no owning agent) must have its in-flight lock released,
        // so it is retried on the next poll instead of being wedged out of
        // due_jobs until restart
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        // Insert a real, claimable DB row under a configured agent, then drive
        // process_due_jobs with an in-memory view whose agent_alias is cleared.
        // With an empty alias and an id bound to no [agents.<x>].cron_jobs list,
        // resolve_owning_agent returns None, so the job is skipped as an orphan.
        let job = cron::add_job(&config, TEST_AGENT, "* * * * *", "echo orphan").unwrap();
        assert!(cron::claim_job(&config, &job.id, Utc::now()).unwrap());
        let orphan = CronJob {
            agent_alias: String::new(),
            ..job.clone()
        };

        process_due_jobs(&config, vec![orphan], &unique_component("orphan"), &None).await;

        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "a skipped orphan job's in-flight lock must be released, not leaked"
        );
    }

    #[tokio::test]
    async fn broadcast_none_skips_without_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo no-broadcast");
        let component = unique_component("broadcast-none");

        // event_tx = None — should complete without panic.
        process_due_jobs(&config, vec![job], &component, &None).await;
    }

    #[tokio::test]
    async fn broadcast_handles_no_subscribers() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo no-subscribers");
        let component = unique_component("broadcast-no-sub");

        let (tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
        // Drop the only receiver immediately — `let _ = tx.send(...)` in
        // process_due_jobs must not panic when there are no subscribers.
        let event_tx: EventBroadcast = Some(tx);

        process_due_jobs(&config, vec![job], &component, &event_tx).await;
        // If we got here without panic, the test passes.
    }
}
