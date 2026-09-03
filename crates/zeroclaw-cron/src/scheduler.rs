use crate::i18n::{get_required_cli_string, get_required_cli_string_with_args};
use crate::precondition::{
    self, PreconditionOutcome, STATUS_PRECONDITION_FAILED, STATUS_SKIPPED_PRECONDITION,
};
use crate::store::{
    RunCompletionAction, persist_manual_run_result, persist_run_completion_state,
    persist_run_result,
};
use crate::{
    CronJob, DeliveryConfig, JobType, Schedule, SessionTarget, all_overdue_jobs, claim_job,
    clear_stale_locks, due_jobs, next_run_for_schedule, release_job, skip_missed_run,
    sync_declarative_jobs,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream};
use std::process::Stdio;
use std::sync::Arc;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::{CronJobDecl, CronScheduleDecl, CronShellOutputFormat};
use zeroclaw_log::Instrument;

/// Action-budget trackers shared across cron runs, keyed by install data
/// directory and agent alias.
///
/// `SecurityPolicy::for_agent` builds a fresh `PerSenderTracker` on every call
/// and cron builds a policy per run, so without a shared tracker the hourly
/// action budget resets each tick and never actually bounds cron work. The
/// data-dir half of the key keeps separate installs (and separate tests) from
/// sharing a budget.
static CRON_ACTION_TRACKERS: std::sync::LazyLock<
    parking_lot::Mutex<
        std::collections::HashMap<
            (std::path::PathBuf, String),
            zeroclaw_config::policy::PerSenderTracker,
        >,
    >,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

/// Build the security policy a cron run executes under.
///
/// Identical to `SecurityPolicy::for_agent` except that the action-budget
/// tracker persists for the life of the process, so `max_actions_per_hour`
/// bounds cron work across runs rather than resetting on each one.
fn cron_security_policy(config: &Config, agent_alias: &str) -> anyhow::Result<SecurityPolicy> {
    let mut policy = SecurityPolicy::for_agent(config, agent_alias)?;
    let key = (config.data_dir.clone(), agent_alias.to_string());
    let tracker = CRON_ACTION_TRACKERS.lock().entry(key).or_default().clone();
    policy.tracker = tracker;
    Ok(policy)
}

const MIN_POLL_SECONDS: u64 = 5;
const SHELL_JOB_TIMEOUT_SECS: u64 = 120;
const SCHEDULER_COMPONENT: &str = "scheduler";
const CRON_AGENT_DEFAULT_EXCLUDED_TOOLS: &[&str] = &[
    "cron_add",
    "cron_update",
    "cron_remove",
    "cron_run",
    "schedule",
];

/// Type alias for the optional broadcast sender used to push cron results
/// to connected dashboard/SSE clients.
pub type EventBroadcast = Option<tokio::sync::broadcast::Sender<serde_json::Value>>;

/// Process health reporter, supplied by the host.
///
/// Registered rather than threaded through every call site, matching how the
/// delivery seam below already works. Unregistered defaults to a no-op, so an
/// embedding that does not run a health registry simply reports nothing
/// instead of failing.
static HEALTH: std::sync::OnceLock<
    std::sync::Arc<dyn zeroclaw_api::cron_traits::CronHealthReporter>,
> = std::sync::OnceLock::new();

/// Register the health reporter. First registration wins.
pub fn register_health_reporter(
    reporter: std::sync::Arc<dyn zeroclaw_api::cron_traits::CronHealthReporter>,
) {
    let _ = HEALTH.set(reporter);
}

fn mark_ok(component: &str) {
    if let Some(h) = HEALTH.get() {
        h.mark_ok(component);
    }
}

fn mark_error(component: &str, reason: &str) {
    if let Some(h) = HEALTH.get() {
        h.mark_error(component, reason);
    }
}

/// Agent-job executor, supplied by the host.
///
/// Cron decides when a job runs and whether policy permits it; the runtime
/// runs the agent. Without a registered executor an agent job fails with a
/// clear message rather than silently reporting success.
static AGENT_EXECUTOR: std::sync::OnceLock<
    std::sync::Arc<dyn zeroclaw_api::cron_traits::CronAgentExecutor>,
> = std::sync::OnceLock::new();

/// Register the agent executor. First registration wins.
pub fn register_agent_executor(
    executor: std::sync::Arc<dyn zeroclaw_api::cron_traits::CronAgentExecutor>,
) {
    let _ = AGENT_EXECUTOR.set(executor);
}

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

/// Result of one cron job execution attempt, including the deterministic
/// precondition gate that runs before the job body.
///
/// The three variants are what keeps "skipped by pre_hook", "failed in
/// pre_hook", and an ordinary job failure distinguishable in run history
/// instead of collapsing into one boolean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CronRunOutcome {
    /// The gate (if any) passed and the job body ran. `success` is the body's
    /// own result.
    Executed { success: bool, output: String },
    /// The gate exited `10`: preconditions not met. The body never started and
    /// this is not a failure.
    SkippedByPrecondition { output: String },
    /// The gate could not authorize the run. The body never started.
    PreconditionFailed { output: String },
}

impl CronRunOutcome {
    /// Constructor for the ordinary `(success, output)` shape.
    fn executed(success: bool, output: String) -> Self {
        Self::Executed { success, output }
    }

    /// `true` unless the run actually failed. A clean precondition skip counts
    /// as a success: the gate did exactly what it was asked to do.
    #[must_use]
    pub(crate) fn is_success(&self) -> bool {
        match self {
            Self::Executed { success, .. } => *success,
            Self::SkippedByPrecondition { .. } => true,
            Self::PreconditionFailed { .. } => false,
        }
    }

    /// `true` when the job body ran; `false` when the gate short-circuited it.
    #[must_use]
    pub(crate) fn ran_body(&self) -> bool {
        matches!(self, Self::Executed { .. })
    }

    /// Run-history status before delivery classification adjusts it.
    #[must_use]
    pub(crate) fn base_status(&self) -> &'static str {
        match self {
            Self::Executed { success: true, .. } => "ok",
            Self::Executed { success: false, .. } => "error",
            Self::SkippedByPrecondition { .. } => STATUS_SKIPPED_PRECONDITION,
            Self::PreconditionFailed { .. } => STATUS_PRECONDITION_FAILED,
        }
    }

    fn into_output(self) -> String {
        match self {
            Self::Executed { output, .. }
            | Self::SkippedByPrecondition { output }
            | Self::PreconditionFailed { output } => output,
        }
    }
}

/// One scheduled run as `process_due_jobs` reports it back to the poll loop.
struct ScheduledRunReport {
    job_id: String,
    status: String,
    success: bool,
    output: String,
}

pub(crate) async fn deliver_and_classify_run_result(
    config: &Config,
    job: &CronJob,
    outcome: CronRunOutcome,
    context: CronDeliveryContext,
) -> CronDeliveryOutcome {
    let mut success = outcome.is_success();
    let mut status = outcome.base_status().to_string();
    let ran_body = outcome.ran_body();
    let skipped_by_precondition = matches!(outcome, CronRunOutcome::SkippedByPrecondition { .. });
    let mut output = outcome.into_output();

    // A clean precondition skip means "nothing to do". Announcing it would put
    // exactly the noise on the channel that the gate exists to avoid, so the
    // skip is recorded in history and goes no further.
    if skipped_by_precondition {
        return CronDeliveryOutcome {
            success,
            status,
            output,
        };
    }

    if let Err(e) = deliver_if_configured(config, job, &output).await {
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
            // A precondition failure keeps its own status: the delivery error
            // is appended to the output, but the run's cause of death is the
            // gate, not the job body.
            if ran_body {
                status = "error".to_string();
            }
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

/// Status recorded when a manual trigger is refused because the job already
/// has an in-flight claim.
pub const STATUS_ALREADY_IN_FLIGHT: &str = "already_in_flight";

/// Holds a cron job's in-flight claim for one run and releases it on every
/// exit path, including an early return, a panic, or a dropped future.
///
/// The scheduled path releases explicitly after `persist_job_result`. The
/// manual path has more exit points than an explicit release can cover, so it
/// uses this guard instead: construct it only after a successful claim, and
/// never before, or a refused trigger would release the claim its competitor
/// is holding.
struct ClaimGuard<'a> {
    config: &'a Config,
    job_id: &'a str,
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = release_job(self.config, self.job_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"job_id": self.job_id, "error": format!("{}", e)})
                    ),
                "manual cron trigger: failed to release in-flight lock"
            );
        }
    }
}

/// Build the result returned when a manual trigger never started a run.
fn manual_refusal(
    job: &CronJob,
    at: DateTime<Utc>,
    status: &str,
    output: String,
) -> ManualCronRunResult {
    ManualCronRunResult {
        job_id: job.id.clone(),
        success: false,
        status: status.to_string(),
        output,
        duration_ms: 0,
        started_at: at,
        finished_at: at,
    }
}

pub async fn run_manual_job(
    config: &Config,
    job: &CronJob,
    context: CronDeliveryContext,
    event_tx: &EventBroadcast,
) -> ManualCronRunResult {
    run_manual_job_inner(config, job, context, event_tx, None, false).await
}

pub async fn run_manual_job_with_runtime(
    config: &Config,
    job: &CronJob,
    context: CronDeliveryContext,
    event_tx: &EventBroadcast,
    runtime: &dyn RuntimeAdapter,
    approved: bool,
) -> ManualCronRunResult {
    run_manual_job_inner(config, job, context, event_tx, Some(runtime), approved).await
}

async fn run_manual_job_inner(
    config: &Config,
    job: &CronJob,
    context: CronDeliveryContext,
    event_tx: &EventBroadcast,
    runtime: Option<&dyn RuntimeAdapter>,
    approved: bool,
) -> ManualCronRunResult {
    let started_at = Utc::now();

    // Claim before the precondition, not just before the body: the claim is
    // the single owner of an execution window, gate included. Without it a due
    // scheduled run and a manual trigger could both pass the same gate and run
    // the body concurrently, which is exactly the non-determinism the gate
    // exists to remove.
    match claim_job(config, &job.id, started_at) {
        Ok(true) => {}
        // The row is locked by another run, or it is gone (a one-shot deleted
        // between the caller's lookup and here). Either way there is no window
        // to own, so refuse instead of starting a second one.
        Ok(false) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"job_id": job.id})),
                "manual cron trigger refused: job already in flight"
            );
            return manual_refusal(
                job,
                started_at,
                STATUS_ALREADY_IN_FLIGHT,
                get_required_cli_string_with_args(
                    "cron-manual-refused-in-flight",
                    &[("id", &job.id)],
                ),
            );
        }
        // Fail closed: if the claim cannot be recorded, exclusivity cannot be
        // proven, so do not run.
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"job_id": job.id, "error": format!("{}", e)})),
                "manual cron trigger refused: failed to claim in-flight lock"
            );
            return manual_refusal(
                job,
                started_at,
                "error",
                get_required_cli_string_with_args(
                    "cron-manual-claim-failed",
                    &[("id", &job.id), ("error", &e.to_string())],
                ),
            );
        }
    }
    // Every path below this point releases the claim when `_claim` drops.
    let _claim = ClaimGuard {
        config,
        job_id: &job.id,
    };

    let run = execute_job_now_with_runtime(config, job, runtime, approved).await;
    let finished_at = Utc::now();
    let duration_ms = (finished_at - started_at).num_milliseconds();
    let outcome = deliver_and_classify_run_result(config, job, run, context).await;

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
            // `status` is what distinguishes a precondition skip (which is
            // deliberately `success: true`) from an ordinary successful run.
            // The scheduled broadcast carries it too; both must agree.
            "status": outcome.status,
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

    mark_ok(SCHEDULER_COMPONENT);

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
            pre_hook: None,
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

    let mut declarative_sync_failed = false;
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
        Err(e) => {
            // Fail closed. A partial reconciliation can leave a row's stored
            // `command`/`prompt` at the previous revision while the gate still
            // resolves from live config, which would authorize an old body with
            // a new precondition. Refusing to run declarative jobs is the only
            // outcome that keeps the gate and the work it authorizes describing
            // the same declaration.
            declarative_sync_failed = true;
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to sync declarative cron jobs; declarative jobs are held until reconciliation succeeds"
            );
        }
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
                mark_ok(SCHEDULER_COMPONENT);

                let jobs = match due_jobs(&config, Utc::now()) {
                    Ok(jobs) => jobs,
                    Err(e) => {
                        mark_error(SCHEDULER_COMPONENT, &e.to_string());
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

                // Held back while reconciliation is unresolved: a declarative
                // row may still carry a body from before the config change the
                // gate is being resolved from.
                let jobs = withhold_declarative_when_unreconciled(jobs, declarative_sync_failed);
                let jobs = claim_due_jobs(&config, jobs);
                process_due_jobs(&config, jobs, SCHEDULER_COMPONENT, &event_tx).await;
            }
            _ = cancel.cancelled() => {
                mark_ok(SCHEDULER_COMPONENT);
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

/// Resolve the single agent whose security policy a job executes under.
///
/// Fails closed on ambiguity. If two enabled agents claim the same cron alias
/// there is no principled way to choose between them, and picking either would
/// let `HashMap` iteration order decide which allowlist, workspace, autonomy
/// level, and action budget a scheduled command runs under -- an order that can
/// change across a restart with no configuration change at all. That is a
/// config error the operator has to resolve, not something to guess at,
/// especially now that the job carries a config-declared `pre_hook`.
fn resolve_owning_agent<'a>(config: &'a Config, job: &CronJob) -> Result<&'a str, String> {
    let owners = super::enabled_cron_owners(config, &job.id);
    if owners.len() > 1 {
        return Err(get_required_cli_string_with_args(
            "cron-owner-ambiguous",
            &[
                ("id", &job.id),
                ("count", &owners.len().to_string()),
                ("owners", &owners.join(", ")),
            ],
        ));
    }

    // A declarative job's owner is whichever agent lists it in `cron_jobs`
    // today, not whichever alias happened to be stored when the row was first
    // synced. The stored alias is not refreshed when config membership moves,
    // so preferring it would run the job -- and its config-declared pre_hook --
    // under the previous owner's allowed commands, workspace roots, autonomy,
    // and action budget. Live config is the source of truth here.
    if job.source == "declarative"
        && let Some(alias) = owners.first()
    {
        return Ok(alias);
    }

    if !job.agent_alias.is_empty()
        && let Some((alias, _)) = config
            .agents
            .iter()
            .find(|(alias, _)| alias.as_str() == job.agent_alias)
    {
        return Ok(alias.as_str());
    }

    owners.first().copied().ok_or_else(|| {
        format!(
            "cron job {id:?} has no owning agent; add the alias to an [agents.<x>].cron_jobs list",
            id = job.id
        )
    })
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

async fn execute_job_now_with_runtime(
    config: &Config,
    job: &CronJob,
    runtime: Option<&dyn RuntimeAdapter>,
    approved: bool,
) -> CronRunOutcome {
    // Reject orphaned declarative jobs: a declarative row whose canonical
    // config declaration has been removed must not execute through any
    // path (automatic polling or manual trigger).
    if job.source == "declarative" && !super::store::is_valid_declarative_owner(config, &job.id) {
        return CronRunOutcome::executed(
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
    let agent_alias = match resolve_owning_agent(config, job) {
        Ok(alias) => alias,
        Err(reason) => return CronRunOutcome::executed(false, reason),
    };
    let agent_alias = agent_alias.to_string();
    let security = match cron_security_policy(config, &agent_alias) {
        Ok(s) => s,
        Err(e) => {
            return CronRunOutcome::executed(
                false,
                format!("agent {agent_alias} risk profile: {e}"),
            );
        }
    };
    let span = zeroclaw_log::attribution_span!(job);
    Box::pin(execute_job_with_retry(
        config,
        &security,
        &agent_alias,
        job,
        runtime,
        approved,
    ))
    .instrument(span)
    .await
}

/// Tools that must not be available to a cron agent job.
///
/// A scheduled run rewriting its own schedule is a foot-gun, so cron excludes
/// the scheduler-mutation tools by default. An explicit `allowed_tools` on the
/// job means the operator has already stated the exact surface, so cron does
/// not add to it.
fn cron_agent_excluded_tools(job: &CronJob) -> Vec<String> {
    if !matches!(job.job_type, JobType::Agent) || job.allowed_tools.is_some() {
        return Vec::new();
    }
    CRON_AGENT_DEFAULT_EXCLUDED_TOOLS
        .iter()
        .map(|t| (*t).to_string())
        .collect()
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
    runtime: Option<&dyn RuntimeAdapter>,
    approved: bool,
) -> CronRunOutcome {
    // A gate is declared in config only, so it is resolved from config rather
    // than from the cron row; agent jobs can carry one just as shell jobs can.
    let pre_hook = precondition::declared_for(config, &job.source, &job.id);

    let needs_runtime = matches!(job.job_type, JobType::Shell) || pre_hook.is_some();
    let owned_runtime = if needs_runtime && runtime.is_none() {
        match zeroclaw_config::platform::create_runtime(&config.runtime) {
            Ok(runtime) => Some(runtime),
            Err(error) => {
                let output = format!("shell setup error: {error}");
                // With a gate declared, an unusable runtime means the gate was
                // never evaluated — fail closed rather than run the body.
                return if pre_hook.is_some() {
                    CronRunOutcome::PreconditionFailed { output }
                } else {
                    CronRunOutcome::executed(false, output)
                };
            }
        }
    } else {
        None
    };
    let runtime = runtime.or(owned_runtime.as_deref());

    // The gate runs once, inside the in-flight claim and before the retry
    // loop. It is a deterministic local check, so retrying it would only ask
    // the same question again.
    if let Some(hook) = pre_hook {
        let Some(runtime) = runtime else {
            return CronRunOutcome::PreconditionFailed {
                output: get_required_cli_string("cron-pre-hook-runtime-missing"),
            };
        };
        match precondition::evaluate(config, runtime, security, hook).await {
            PreconditionOutcome::Proceed => {}
            PreconditionOutcome::Skip { output } => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "job_id": job.id,
                            "agent_alias": agent_alias,
                            "output": output
                        })),
                    "Cron job skipped by pre_hook precondition"
                );
                return CronRunOutcome::SkippedByPrecondition { output };
            }
            PreconditionOutcome::Failed { output } => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "job_id": job.id,
                            "agent_alias": agent_alias,
                            "output": output
                        })),
                    "Cron job pre_hook precondition failed"
                );
                return CronRunOutcome::PreconditionFailed { output };
            }
        }
    }

    let mut last_output = String::new();
    let retries = config.reliability.scheduler_retries;
    let mut backoff_ms = config.reliability.provider_backoff_ms.max(200);

    for attempt in 0..=retries {
        let (success, output) = match job.job_type {
            JobType::Shell => {
                let Some(runtime) = runtime else {
                    return CronRunOutcome::executed(
                        false,
                        "shell setup error: runtime missing for shell cron job".to_string(),
                    );
                };
                run_job_command_with_runtime(config, runtime, security, job, approved).await
            }
            JobType::Agent => Box::pin(run_agent_job(config, security, agent_alias, job)).await,
        };
        last_output = output;

        if success {
            return CronRunOutcome::executed(true, last_output);
        }

        if last_output.starts_with("blocked by security policy:") {
            // Deterministic policy violations are not retryable.
            return CronRunOutcome::executed(false, last_output);
        }

        if attempt < retries {
            let jitter_ms = u64::from(Utc::now().timestamp_subsec_millis() % 250);
            time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms.saturating_mul(2)).min(30_000);
        }
    }

    CronRunOutcome::executed(false, last_output)
}

/// Drop declarative jobs from a due batch when declarative reconciliation has
/// not succeeded this process.
///
/// Imperative jobs are unaffected: their body lives on the row and has no
/// config declaration to disagree with.
fn withhold_declarative_when_unreconciled(jobs: Vec<CronJob>, sync_failed: bool) -> Vec<CronJob> {
    if !sync_failed {
        return jobs;
    }
    jobs.into_iter()
        .filter(|job| {
            if job.source == "declarative" {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"job_id": job.id})),
                    "Skipping declarative cron job: config reconciliation has not succeeded"
                );
                return false;
            }
            true
        })
        .collect()
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
    mark_ok(component);

    let max_concurrent = config.scheduler.max_concurrent.max(1);
    let mut in_flight = stream::iter(jobs.into_iter().filter_map(|job| {
        let agent_alias = match resolve_owning_agent(config, &job) {
            Ok(alias) => alias,
            Err(reason) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"job_id": job.id, "reason": reason})), "Cron job owner unresolved; refusing to run");
                let _ = release_job(config, &job.id);
                return None;
            }
        };
        let agent_alias = agent_alias.to_owned();
        let security = match cron_security_policy(config, &agent_alias) {
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

    while let Some(report) = in_flight.next().await {
        if !report.success {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "job_id": report.job_id,
                        "status": report.status,
                        "output": report.output
                    })),
                "Scheduler job '' failed: "
            );
        }
        // Broadcast cron result to dashboard/SSE clients. `status` carries the
        // distinction a bare `success` flag cannot: a precondition skip is not
        // a plain success and a precondition failure is not a plain error.
        if let Some(tx) = event_tx {
            let _ = tx.send(serde_json::json!({
                "type": "cron_result",
                "job_id": report.job_id,
                "status": report.status,
                "success": report.success,
                "output": report.output,
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
) -> ScheduledRunReport {
    mark_ok(component);
    warn_if_high_frequency_agent_job(job);

    let started_at = Utc::now();
    let span = zeroclaw_log::attribution_span!(job);
    let run = Box::pin(execute_job_with_retry(
        config,
        security,
        agent_alias,
        job,
        None,
        false,
    ))
    .instrument(span)
    .await;
    let finished_at = Utc::now();
    let outcome = Box::pin(persist_job_result(
        config,
        job,
        run,
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

    ScheduledRunReport {
        job_id: job.id.clone(),
        status: outcome.status,
        success: outcome.success,
        output: outcome.output,
    }
}

async fn run_agent_job(
    config: &Config,
    security: &SecurityPolicy,
    agent_alias: &str,
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

    // Everything above is cron's own business: policy admission, the action
    // budget, the prompt envelope, and which session path the run belongs to.
    // Executing the agent is not, so it goes across the seam.
    //
    // The request carries policy *inputs* rather than a built `SecurityPolicy`.
    // `zeroclaw-api` is the leaf trait crate and cannot see `zeroclaw-config`
    // types, and inverting that would be a dependency cycle. The host rebuilds
    // the effective policy from the alias and these narrowing lists.
    let request = zeroclaw_api::cron_traits::CronAgentRequest {
        job_id: job.id.clone(),
        agent_alias: agent_alias.to_string(),
        prompt: prefixed_prompt,
        model: model_override,
        session_path,
        allowed_tools: job.allowed_tools.clone(),
        workspace_dir: security.workspace_dir.clone(),
        excluded_tools: cron_agent_excluded_tools(job),
        uses_memory: job.uses_memory,
    };

    let Some(executor) = AGENT_EXECUTOR.get() else {
        // Fail loudly rather than report a success nobody performed.
        return (
            false,
            "agent job failed: no cron agent executor registered \
             (register_agent_executor was not called by the host)"
                .to_string(),
        );
    };

    let run = executor.run_agent_job(request).await;
    (run.success, run.output)
}

async fn persist_job_result(
    config: &Config,
    job: &CronJob,
    run: CronRunOutcome,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) -> CronDeliveryOutcome {
    let duration_ms = (finished_at - started_at).num_milliseconds();
    let ran_body = run.ran_body();
    let outcome =
        deliver_and_classify_run_result(config, job, run, CronDeliveryContext::Scheduled).await;

    // Auto-delete is the reward for a one-shot that actually did its work. A
    // one-shot whose gate skipped it never ran, so it is disabled instead —
    // that keeps the skip visible in history rather than erasing the row.
    let action = if is_one_shot_auto_delete(job) && ran_body && outcome.success {
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

    outcome
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

async fn run_job_command_with_runtime(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    job: &CronJob,
    approved: bool,
) -> (bool, String) {
    run_job_command_with_runtime_and_timeout(
        config,
        runtime,
        security,
        job,
        approved,
        Duration::from_secs(SHELL_JOB_TIMEOUT_SECS),
    )
    .await
}

async fn run_job_command_with_runtime_and_timeout(
    config: &Config,
    runtime: &dyn RuntimeAdapter,
    security: &SecurityPolicy,
    job: &CronJob,
    approved: bool,
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
    if let Err(error) =
        crate::validate_shell_command_with_security(runtime, security, &job.command, approved)
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

    let mut command = match runtime.build_shell_command(&job.command, &config.data_dir) {
        Ok(command) => command,
        Err(error) => return (false, format!("shell setup error: {error}")),
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return (false, format!("spawn error: {error}")),
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

#[cfg(test)]
async fn run_job_command(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    let runtime = match zeroclaw_config::platform::create_runtime(&config.runtime) {
        Ok(runtime) => runtime,
        Err(error) => return (false, format!("shell setup error: {error}")),
    };
    run_job_command_with_runtime(config, runtime.as_ref(), security, job, false).await
}

#[cfg(all(test, not(target_os = "windows")))]
async fn run_job_command_with_timeout(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    timeout: Duration,
) -> (bool, String) {
    let runtime = match zeroclaw_config::platform::create_runtime(&config.runtime) {
        Ok(runtime) => runtime,
        Err(error) => return (false, format!("shell setup error: {error}")),
    };
    run_job_command_with_runtime_and_timeout(
        config,
        runtime.as_ref(),
        security,
        job,
        false,
        timeout,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    // These tests moved in with the code and still address the module by its
    // old name. Aliasing keeps them readable rather than rewriting hundreds of
    // call sites; without it `cron::` resolves to the schedule-parsing crate.
    use crate as cron;

    /// Records what the scheduler reports, standing in for the host registry.
    ///
    /// Registration is first-wins and the whole test binary shares one
    /// process, so this is installed once and read by every health test.
    #[derive(Default)]
    struct RecordingHealth {
        ok: parking_lot::Mutex<Vec<String>>,
        errors: parking_lot::Mutex<Vec<String>>,
    }

    static RECORDING_HEALTH: std::sync::LazyLock<std::sync::Arc<RecordingHealth>> =
        std::sync::LazyLock::new(|| {
            let reporter = std::sync::Arc::new(RecordingHealth::default());
            register_health_reporter(reporter.clone());
            reporter
        });

    impl zeroclaw_api::cron_traits::CronHealthReporter for RecordingHealth {
        fn mark_ok(&self, component: &str) {
            self.ok.lock().push(component.to_string());
        }
        fn mark_error(&self, component: &str, _reason: &str) {
            self.errors.lock().push(component.to_string());
        }
    }

    fn recorded_health_ok() -> Vec<String> {
        RECORDING_HEALTH.ok.lock().clone()
    }

    fn recorded_health_errors() -> Vec<String> {
        RECORDING_HEALTH.errors.lock().clone()
    }
    use crate::DeliveryConfig;
    use chrono::{Duration as ChronoDuration, Utc};
    use tempfile::TempDir;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::{Config, RuntimeKind};

    const TEST_AGENT: &str = "test-agent";

    fn build_configured_shell_command(
        config: &Config,
        command: &str,
        workspace_dir: &std::path::Path,
    ) -> anyhow::Result<tokio::process::Command> {
        let runtime = zeroclaw_config::platform::create_runtime(&config.runtime)?;
        runtime.build_shell_command(command, workspace_dir)
    }

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
            schedule: crate::Schedule::Cron {
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

    struct PowerShellProbeRuntime {
        build_calls: std::sync::atomic::AtomicUsize,
    }

    impl PowerShellProbeRuntime {
        fn new() -> Self {
            Self {
                build_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl RuntimeAdapter for PowerShellProbeRuntime {
        fn name(&self) -> &str {
            "powershell-probe"
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }

        fn supports_long_running(&self) -> bool {
            true
        }

        fn shell_dialect(&self) -> zeroclaw_api::runtime_traits::ShellDialect {
            zeroclaw_api::runtime_traits::ShellDialect::PowerShell
        }

        fn build_shell_command(
            &self,
            _command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            self.build_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            #[cfg(target_os = "windows")]
            let mut command = {
                let mut command = tokio::process::Command::new("cmd");
                command.args(["/C", "echo", "same-runtime"]);
                command
            };

            #[cfg(not(target_os = "windows"))]
            let mut command = {
                let mut command = tokio::process::Command::new("printf");
                command.arg("same-runtime");
                command
            };

            command.current_dir(workspace_dir);
            Ok(command)
        }
    }

    #[tokio::test]
    async fn cron_shell_validation_and_execution_share_runtime_adapter() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let security = SecurityPolicy {
            autonomy: zeroclaw_config::policy::AutonomyLevel::Full,
            workspace_dir: config.data_dir.clone(),
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };
        let runtime = PowerShellProbeRuntime::new();

        let safe_job = test_job("Write-Output \"quoted safe value\" | Select-Object -First 1");
        let (success, output) =
            run_job_command_with_runtime(&config, &runtime, &security, &safe_job, false).await;
        assert!(success, "{output}");
        assert!(output.contains("same-runtime"), "{output}");
        assert_eq!(
            runtime
                .build_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        let dangerous_job = test_job("ac blocked.txt value");
        let (success, output) =
            run_job_command_with_runtime(&config, &runtime, &security, &dangerous_job, true).await;
        assert!(!success);
        assert!(output.contains("high-risk"), "{output}");
        assert_eq!(
            runtime
                .build_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "policy rejection must happen before the runtime builds a command"
        );
    }

    fn unique_component(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    fn agent_job_with_schedule(schedule: crate::Schedule) -> CronJob {
        CronJob {
            job_type: JobType::Agent,
            schedule,
            ..test_job("echo test")
        }
    }

    #[test]
    fn high_frequency_daily_cron_is_not_flagged() {
        // `0 6 * * *` fires once per day — must never warn regardless of when the check runs
        let job = agent_job_with_schedule(crate::Schedule::Cron {
            expr: "0 6 * * *".into(),
            tz: Some("America/Chicago".into()),
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_4min_cron_is_flagged() {
        let job = agent_job_with_schedule(crate::Schedule::Cron {
            expr: "*/4 * * * *".into(),
            tz: None,
        });
        assert!(is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_5min_cron_is_not_flagged() {
        // Exactly 5 minutes is acceptable (threshold is strictly less than 5)
        let job = agent_job_with_schedule(crate::Schedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_interval_below_threshold_is_flagged() {
        let job = agent_job_with_schedule(crate::Schedule::Every {
            every_ms: 4 * 60 * 1000, // 4 minutes
        });
        assert!(is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_every_interval_at_threshold_is_not_flagged() {
        let job = agent_job_with_schedule(crate::Schedule::Every {
            every_ms: 5 * 60 * 1000, // exactly 5 minutes
        });
        assert!(!is_high_frequency_agent_job(&job));
    }

    #[test]
    fn high_frequency_shell_job_is_never_flagged() {
        // Shell jobs are exempt regardless of frequency
        let job = CronJob {
            job_type: JobType::Shell,
            schedule: crate::Schedule::Every {
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
    fn cron_excludes_scheduler_mutation_tools_from_agent_jobs_by_default() {
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.allowed_tools = None;

        let excluded = cron_agent_excluded_tools(&job);

        for tool in [
            "cron_add",
            "cron_update",
            "cron_remove",
            "cron_run",
            "schedule",
        ] {
            assert!(
                excluded.iter().any(|t| t == tool),
                "{tool} must be excluded from default cron agent runs"
            );
        }
        // Cron names only what it wants removed. Everything else is the
        // agent profile's business, and cron does not speak for it.
        assert!(!excluded.iter().any(|t| t == "http_request"));
    }

    #[test]
    fn cron_adds_no_exclusions_when_the_job_names_its_own_tools() {
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.allowed_tools = Some(vec!["cron_add".into()]);

        // An explicit allowlist means the operator already stated the exact
        // surface, including deliberate scheduler automation. Cron does not
        // second-guess it.
        assert!(cron_agent_excluded_tools(&job).is_empty());
    }

    #[test]
    fn cron_adds_no_exclusions_for_shell_jobs() {
        let job = test_job("echo hi");
        assert!(matches!(job.job_type, JobType::Shell));
        assert!(cron_agent_excluded_tools(&job).is_empty());
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
    async fn run_job_command_blocks_windows_relative_path_for_powershell() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["cat".into()];
        let job = test_job("cat ..\\secret.txt");
        let security = test_security(&config);
        let runtime = zeroclaw_config::platform::NativeRuntime::with_shell("pwsh".into());

        let (success, output) =
            run_job_command_with_runtime(&config, &runtime, &security, &job, false).await;

        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("..\\secret.txt"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_powershell_stop_parsing_native_mutation() {
        // Cron shares the same dialect-aware validator as the shell tool. On a
        // PowerShell runtime, `git --% push` would strip `--%` and hand `push`
        // to native Git while policy only sees `--%`; the bounded grammar must
        // reject it so scheduled jobs cannot launder mutations through it.
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["git".into()];
        let job = test_job("git --% push origin main");
        let security = test_security(&config);
        let runtime = zeroclaw_config::platform::NativeRuntime::with_shell("pwsh".into());

        let (success, output) =
            run_job_command_with_runtime(&config, &runtime, &security, &job, false).await;

        assert!(!success);
        assert!(
            output.contains("blocked by security policy"),
            "output: {output}"
        );
    }

    #[tokio::test]
    async fn run_job_command_blocks_powershell_mixed_quoted_provider_path() {
        // A scheduled job must not launder an `Env:` provider read past policy
        // by splitting the provider prefix with a quote: `cat E'nv:'PATH` binds
        // as `Env:PATH` on PowerShell. The bounded grammar rejects the mixed
        // quoted/unquoted token through the same validator cron uses.
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["cat".into()];
        let job = test_job("cat E'nv:'PATH");
        let security = test_security(&config);
        let runtime = zeroclaw_config::platform::NativeRuntime::with_shell("pwsh".into());

        let (success, output) =
            run_job_command_with_runtime(&config, &runtime, &security, &job, false).await;

        assert!(!success);
        assert!(
            output.contains("blocked by security policy"),
            "output: {output}"
        );
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
            .level = zeroclaw_config::policy::AutonomyLevel::ReadOnly;
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

        let outcome = Box::pin(execute_job_with_retry(
            &config,
            &security,
            "test-agent",
            &job,
            None,
            false,
        ))
        .await;
        let (success, output) = (outcome.is_success(), outcome.into_output());
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

        let outcome = Box::pin(execute_job_with_retry(
            &config,
            &security,
            "test-agent",
            &job,
            None,
            false,
        ))
        .await;
        let (success, output) = (outcome.is_success(), outcome.into_output());
        assert!(!success);
        assert!(output.contains("always_missing_for_retry_test"));
    }

    /// Cron reports an executor failure as a job failure.
    ///
    /// Replaces a pre-extraction test that asserted an agent run fails without
    /// a provider key. Why the run failed is the host executor's business now;
    /// what cron still owns is not swallowing that failure or reporting it as
    /// success.
    #[tokio::test]
    async fn agent_job_failure_from_the_executor_is_reported_as_failure() {
        std::sync::LazyLock::force(&RECORDING_EXECUTOR);
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some(format!("Say hello {EXECUTOR_FAILURE_SENTINEL}"));
        let security = test_security(&config);

        let (success, output) =
            Box::pin(run_agent_job(&config, &security, "test-agent", &job)).await;
        assert!(!success);
        assert!(output.contains("agent job failed:"), "unexpected: {output}");
    }

    /// Prompt marker that makes the stub executor report failure.
    const EXECUTOR_FAILURE_SENTINEL: &str = "__cron_stub_should_fail__";

    /// Records what cron hands across the executor seam.
    struct RecordingExecutor {
        seen: std::sync::Arc<parking_lot::Mutex<Vec<zeroclaw_api::cron_traits::CronAgentRequest>>>,
    }

    impl zeroclaw_api::cron_traits::CronAgentExecutor for RecordingExecutor {
        fn run_agent_job<'a>(
            &'a self,
            request: zeroclaw_api::cron_traits::CronAgentRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = zeroclaw_api::cron_traits::CronAgentRun>
                    + Send
                    + 'a,
            >,
        > {
            let fail = request.prompt.contains(EXECUTOR_FAILURE_SENTINEL);
            self.seen.lock().push(request);
            Box::pin(async move {
                if fail {
                    zeroclaw_api::cron_traits::CronAgentRun {
                        success: false,
                        output: "agent job failed: stub executor was asked to fail".to_string(),
                    }
                } else {
                    zeroclaw_api::cron_traits::CronAgentRun {
                        success: true,
                        output: "done".to_string(),
                    }
                }
            })
        }
    }

    static RECORDING_EXECUTOR: std::sync::LazyLock<
        std::sync::Arc<parking_lot::Mutex<Vec<zeroclaw_api::cron_traits::CronAgentRequest>>>,
    > = std::sync::LazyLock::new(|| {
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        register_agent_executor(std::sync::Arc::new(RecordingExecutor {
            seen: seen.clone(),
        }));
        seen
    });

    /// The scheduler's workspace must survive the crate boundary.
    ///
    /// Ported from the pre-extraction test added by the scheduler-workspace
    /// fix. That test asserted end to end that shell output came from the
    /// scheduler's workspace; agent execution now lives behind the executor
    /// seam, so this asserts the half cron still owns: that the resolved
    /// workspace is what cron hands over, on the retry path and under
    /// concurrency. The end-to-end half belongs with the host executor.
    #[tokio::test]
    async fn cron_hands_the_scheduler_workspace_across_the_executor_seam() {
        let seen = std::sync::LazyLock::force(&RECORDING_EXECUTOR).clone();
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;

        let mut security = test_security(&config);
        let scheduler_workspace = tmp.path().join("scheduler-owned-workspace");
        std::fs::create_dir_all(&scheduler_workspace).unwrap();
        security.workspace_dir = scheduler_workspace.clone();
        assert_ne!(
            scheduler_workspace,
            config.agent_workspace_dir(TEST_AGENT),
            "the test is only meaningful when the scheduler workspace differs from the agent default"
        );

        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Print the current workspace directory".into());
        job.allowed_tools = Some(vec!["shell".into()]);
        job.uses_memory = false;

        let before = seen.lock().len();

        let outcome = Box::pin(execute_job_with_retry(
            &config, &security, TEST_AGENT, &job, None, false,
        ))
        .await;
        assert!(outcome.is_success(), "{}", outcome.clone().into_output());

        let (a, b, c) = tokio::join!(
            run_agent_job(&config, &security, TEST_AGENT, &job),
            run_agent_job(&config, &security, TEST_AGENT, &job),
            run_agent_job(&config, &security, TEST_AGENT, &job),
        );
        for result in [a, b, c] {
            assert!(result.0, "concurrent cron agent run failed: {:?}", result.1);
        }

        let requests = seen.lock();
        let ours = &requests[before..];
        assert_eq!(ours.len(), 4, "one request per run, retries included");
        for request in ours {
            assert_eq!(
                request.workspace_dir, scheduler_workspace,
                "the scheduler workspace must not be replaced by the agent default"
            );
            assert_eq!(request.agent_alias, TEST_AGENT);
        }
    }

    #[tokio::test]
    async fn run_agent_job_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = zeroclaw_config::policy::AutonomyLevel::ReadOnly;
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
        // Force reporter registration before anything can report.
        std::sync::LazyLock::force(&RECORDING_HEALTH);
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let component = unique_component("idle-health");

        // An idle poll still has to report liveness, otherwise a silent
        // scheduler is indistinguishable from one with nothing to do.
        process_due_jobs(&config, vec![], &component, &None).await;

        assert!(
            recorded_health_ok().iter().any(|c| c == &component),
            "an idle poll must still mark the component healthy"
        );
    }

    #[tokio::test]
    async fn process_due_jobs_failure_does_not_mark_component_unhealthy() {
        // Force reporter registration before anything can report.
        std::sync::LazyLock::force(&RECORDING_HEALTH);
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let component = unique_component("failure-health");
        let job = test_job("definitely_not_a_real_command_xyz");

        process_due_jobs(&config, vec![job], &component, &None).await;

        // A failing job is a job problem, not a scheduler problem. The
        // scheduler completed its poll, so it stays healthy.
        assert!(recorded_health_ok().iter().any(|c| c == &component));
        assert!(
            !recorded_health_errors().iter().any(|c| c == &component),
            "a failed job must not mark the scheduler itself unhealthy"
        );
    }

    #[tokio::test]
    async fn persist_job_result_records_run_and_reschedules_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
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

        crate::store::reset_write_connection_count_for_tests(&config);
        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;

        assert!(success);
        assert_eq!(crate::store::write_connection_count_for_tests(&config), 1);
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

            let success = persist_job_result(
                &config,
                &job,
                CronRunOutcome::executed(true, output.clone()),
                started,
                finished,
            )
            .await
            .success;
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

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;

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
            crate::Schedule::At { at },
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

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
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
            crate::Schedule::At { at },
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

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(false, "boom".into()),
            started,
            finished,
        )
        .await
        .success;
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
            crate::Schedule::At { at },
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

        crate::store::reset_write_connection_count_for_tests(&config);
        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(false, "boom".into()),
            started,
            finished,
        )
        .await
        .success;

        assert!(!success);
        assert_eq!(crate::store::write_connection_count_for_tests(&config), 1);
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

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
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
        let job =
            cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell", None).unwrap();
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

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
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
        let job =
            cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell", None).unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
        assert!(success);
        let lookup = cron::get_job(&config, &job.id);
        assert!(lookup.is_err());
    }

    #[tokio::test]
    async fn persist_job_result_failure_disables_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let job =
            cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell", None).unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(false, "boom".into()),
            started,
            finished,
        )
        .await
        .success;
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
            crate::Schedule::Cron {
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

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
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

        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
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
            CronRunOutcome::executed(true, String::new()),
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
            crate::Schedule::At { at },
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
        let success = persist_job_result(
            &config,
            &job,
            CronRunOutcome::executed(true, "ok".into()),
            started,
            finished,
        )
        .await
        .success;
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
    #[cfg(not(target_os = "windows"))]
    fn build_cron_shell_command_uses_configured_runtime() {
        let config = Config::default();
        let workspace = std::env::temp_dir();
        let cmd = build_configured_shell_command(&config, "echo cron-test", &workspace).unwrap();
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
    #[cfg(not(target_os = "windows"))]
    async fn build_cron_shell_command_executes_successfully() {
        let config = Config::default();
        let workspace = std::env::temp_dir();
        let mut cmd = build_configured_shell_command(&config, "echo cron-ok", &workspace).unwrap();
        let output = cmd.output().await.unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("cron-ok"));
    }

    #[tokio::test]
    #[cfg(all(unix, not(target_os = "android")))]
    async fn build_cron_shell_command_executes_with_custom_native_shell() {
        let tmp = TempDir::new().unwrap();
        let shim = tmp.path().join("cron-shell-shim");
        // Avoid writing an executable after the test process is multithreaded:
        // a concurrently forked child can inherit the write descriptor and
        // make the subsequent exec fail with ETXTBSY.
        let shell = which::which("sh").unwrap();
        std::os::unix::fs::symlink(shell, &shim).unwrap();

        let mut config = Config::default();
        config.runtime.shell = Some(shim.to_string_lossy().into_owned());
        let mut cmd = build_configured_shell_command(
            &config,
            "printf 'CUSTOM_SHELL:%s\\n' \"$0\"",
            tmp.path(),
        )
        .unwrap();
        let output = cmd.output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success());
        assert_eq!(stdout.trim(), format!("CUSTOM_SHELL:{}", shim.display()));
    }

    #[test]
    fn build_cron_shell_command_preserves_docker_runtime_boundary() {
        let mut config = Config::default();
        config.runtime.kind = RuntimeKind::Docker;
        config.runtime.docker.image = "alpine:3.20".into();
        config.runtime.docker.network = "none".into();
        config.runtime.docker.mount_workspace = false;

        let cmd =
            build_configured_shell_command(&config, "echo cron-docker", &std::env::temp_dir())
                .unwrap();
        let debug = format!("{cmd:?}");

        assert!(debug.contains("\"docker\""), "{debug}");
        assert!(debug.contains("\"run\""), "{debug}");
        assert!(debug.contains("\"--network\""), "{debug}");
        assert!(debug.contains("\"none\""), "{debug}");
        assert!(debug.contains("\"alpine:3.20\""), "{debug}");
        assert!(
            debug.contains("\"sh\" \"-c\" \"echo cron-docker\""),
            "{debug}"
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn build_cron_shell_command_uses_configured_powershell() {
        let mut config = Config::default();
        config.runtime.shell = Some("powershell".into());
        let workspace = std::env::temp_dir();
        let cmd =
            build_configured_shell_command(&config, "Write-Output cron-ok", &workspace).unwrap();
        let debug = format!("{cmd:?}");
        assert!(debug.contains("powershell"));
        assert!(debug.contains("-Command"));
        assert!(!debug.contains("cmd.exe"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn cron_powershell_policy_accepts_read_only_and_rejects_expressions() {
        let mut config = Config::default();
        config.runtime.shell = Some("powershell".into());
        // PowerShell-only command names are deliberately absent from the
        // cross-dialect default allowlist (see
        // `docs/book/src/security/sandboxing.md`): an operator opts into the
        // cmdlets they need. Grant both documented spellings so the assertions
        // below exercise the PowerShell grammar rather than the allowlist.
        let security = SecurityPolicy {
            allowed_commands: vec!["Write-Output".into(), "echo".into()],
            ..SecurityPolicy::default()
        };
        let runtime = zeroclaw_config::platform::create_runtime(&config.runtime).unwrap();

        crate::validate_shell_command_with_security(
            runtime.as_ref(),
            &security,
            "Write-Output $PSHOME",
            false,
        )
        .expect("documented read-only PowerShell command should pass");
        assert!(
            crate::validate_shell_command_with_security(
                runtime.as_ref(),
                &security,
                "echo ([System.IO.File]::Delete('important.txt'))",
                false,
            )
            .is_err()
        );
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

    // ── Precondition gate (`[cron.<alias>.pre_hook]`) ─────────────────

    /// Marker the gate tests' job bodies echo. Its presence in the recorded
    /// output is the evidence that the body actually ran.
    const GATED_BODY_MARKER: &str = "gated-body-ran";

    /// Declare a shell job with a gate in config, sync it, and return the row
    /// the scheduler would pick up.
    fn declarative_gated_job(
        config: &mut Config,
        id: &str,
        pre_hook_command: &str,
        timeout_secs: u64,
    ) -> CronJob {
        use zeroclaw_config::schema::{CronJobDecl, CronPreHookDecl, CronScheduleDecl};

        config.cron.insert(
            id.to_string(),
            CronJobDecl {
                job_type: "shell".into(),
                schedule: CronScheduleDecl::Cron {
                    expr: "*/5 * * * *".into(),
                    tz: None,
                },
                command: Some(format!("echo {GATED_BODY_MARKER}")),
                pre_hook: Some(CronPreHookDecl {
                    command: pre_hook_command.into(),
                    timeout_secs,
                }),
                ..CronJobDecl::default()
            },
        );
        config
            .agents
            .get_mut(TEST_AGENT)
            .expect("test agent exists")
            .cron_jobs
            .push(id.to_string());

        let decls = config.cron.clone();
        cron::sync_declarative_jobs(config, &decls).expect("declarative sync should succeed");
        cron::get_job(config, id).expect("synced job should be readable")
    }

    /// Allow only what the gate tests use: `exit`/`sleep` for the hook and
    /// `echo` for the body. Anything else the gate reaches for must be refused.
    fn allow_gate_test_commands(config: &mut Config) {
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["exit".into(), "sleep".into(), "echo".into()];
    }

    /// Evidence that the gated job body ran, taken from what was recorded.
    fn body_ran(result: &ManualCronRunResult) -> bool {
        result.output.contains(GATED_BODY_MARKER)
    }

    #[tokio::test]
    async fn pre_hook_exit_zero_runs_the_job_and_records_ok() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "gate-proceed", "exit 0", 30);

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;

        assert!(
            result.success,
            "exit 0 should let the job run: {}",
            result.output
        );
        assert_eq!(result.status, "ok");
        assert!(body_ran(&result), "job body should have run");

        let runs = cron::list_runs(&config, &job.id, 10).expect("run history should list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn pre_hook_exit_ten_records_a_clean_skip_and_never_starts_the_job() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "gate-skip", "exit 10", 30);

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;

        // A clean skip is not a failure.
        assert!(
            result.success,
            "a precondition skip must not report failure"
        );
        assert_eq!(result.status, STATUS_SKIPPED_PRECONDITION);
        assert!(result.output.contains("pre_hook requested skip (exit 10)"));
        assert!(!body_ran(&result), "job body must not run after a skip");

        // The skip is distinguishable in history, not folded into "ok".
        let runs = cron::list_runs(&config, &job.id, 10).expect("run history should list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, STATUS_SKIPPED_PRECONDITION);

        let updated = cron::get_job(&config, &job.id).expect("job state should update");
        assert_eq!(
            updated.last_status.as_deref(),
            Some(STATUS_SKIPPED_PRECONDITION)
        );
    }

    #[tokio::test]
    async fn pre_hook_other_nonzero_exit_records_a_precondition_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "gate-fail", "exit 3", 30);

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;

        assert!(!result.success, "a failing gate is a failed run");
        assert_eq!(result.status, STATUS_PRECONDITION_FAILED);
        assert!(result.output.contains("pre_hook failed (exit 3)"));
        assert!(
            !body_ran(&result),
            "job body must not run after a gate failure"
        );

        // Distinct from both "ok" and a plain "error" job failure.
        let runs = cron::list_runs(&config, &job.id, 10).expect("run history should list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, STATUS_PRECONDITION_FAILED);
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn pre_hook_timeout_records_a_precondition_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "gate-timeout", "sleep 30", 1);

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;

        assert!(!result.success, "a timed-out gate is a failed run");
        assert_eq!(result.status, STATUS_PRECONDITION_FAILED);
        assert!(
            result.output.contains("pre_hook timed out after 1s"),
            "unexpected output: {}",
            result.output
        );
        assert!(
            !body_ran(&result),
            "job body must not run after a gate timeout"
        );
    }

    #[tokio::test]
    async fn pre_hook_blocked_by_security_policy_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        // `curl` is not in the allowlist: the gate is refused, and a refused
        // gate must be a loud failure rather than a quiet skip.
        let job = declarative_gated_job(
            &mut config,
            "gate-blocked",
            "curl https://example.invalid",
            30,
        );

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;

        assert!(!result.success);
        assert_eq!(result.status, STATUS_PRECONDITION_FAILED);
        assert!(
            result.output.contains("blocked by security policy"),
            "unexpected output: {}",
            result.output
        );
        assert!(
            !body_ran(&result),
            "job body must not run when the gate is refused"
        );
    }

    #[tokio::test]
    async fn pre_hook_in_config_does_not_gate_a_same_id_imperative_job() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["echo".into(), "exit".into()];

        // A declarative entry that would skip everything…
        declarative_gated_job(&mut config, "shared-id", "exit 10", 30);

        // …must not attach itself to an imperative job that shares its id.
        // Imperative jobs have no gate: only config can declare one.
        let mut job = test_job("echo imperative-body-ran");
        job.id = "shared-id".into();
        job.source = "imperative".into();

        let outcome = execute_job_now_with_runtime(&config, &job, None, false).await;

        assert!(outcome.ran_body(), "imperative job should not be gated");
        assert!(outcome.is_success());
        assert!(outcome.into_output().contains("imperative-body-ran"));
    }

    #[tokio::test]
    async fn precondition_skip_is_recorded_but_never_announced() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        // announce mode with no channel: any delivery attempt fails loudly.
        let mut job = test_job("echo unused");
        job.delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: None,
            to: None,
            thread_id: None,
            best_effort: false,
        };

        let skipped = deliver_and_classify_run_result(
            &config,
            &job,
            CronRunOutcome::SkippedByPrecondition {
                output: "pre_hook requested skip (exit 10)".into(),
            },
            CronDeliveryContext::Scheduled,
        )
        .await;

        assert!(skipped.success);
        assert_eq!(skipped.status, STATUS_SKIPPED_PRECONDITION);
        assert!(
            !skipped.output.contains("delivery failed"),
            "a clean skip must not enter the delivery path: {}",
            skipped.output
        );

        // Control: an executed run with the same delivery config does try to
        // deliver, so the assertion above is testing the skip, not the config.
        let executed = deliver_and_classify_run_result(
            &config,
            &job,
            CronRunOutcome::Executed {
                success: true,
                output: "real output".into(),
            },
            CronDeliveryContext::Scheduled,
        )
        .await;
        assert!(executed.output.contains("delivery failed"));
    }

    #[tokio::test]
    async fn precondition_failure_keeps_its_status_through_delivery_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("echo unused");
        job.delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: None,
            to: None,
            thread_id: None,
            best_effort: false,
        };

        let outcome = deliver_and_classify_run_result(
            &config,
            &job,
            CronRunOutcome::PreconditionFailed {
                output: "pre_hook failed (exit 3)".into(),
            },
            CronDeliveryContext::Scheduled,
        )
        .await;

        assert!(!outcome.success);
        // The delivery error is appended, but the cause of death stays the gate.
        assert_eq!(outcome.status, STATUS_PRECONDITION_FAILED);
        assert!(outcome.output.contains("delivery failed"));
    }

    // ── Startup recovery, reconciliation, and budget lifetime ────────

    #[tokio::test]
    async fn startup_recovery_keeps_a_claim_made_by_this_process() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "own-claim", "exit 0", 30);

        // The gateway accepted a manual trigger before the scheduler started.
        assert!(cron::claim_job(&config, &job.id, Utc::now()).unwrap());

        // Scheduler startup recovery now runs in the same process.
        let cleared = cron::clear_stale_locks(&config).expect("recovery should succeed");

        assert_eq!(cleared, 0, "this process's own claim must not be cleared");
        assert!(
            !cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "the live claim must still be held after startup recovery"
        );
    }

    #[tokio::test]
    async fn startup_recovery_clears_a_claim_left_by_another_process() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "dead-claim", "exit 0", 30);

        // A lock whose owner is some other (dead) process, and the pre-column
        // shape where the owner is unknown. Both are stale by definition.
        for owner in [Some("some-other-process"), None] {
            cron::test_support::force_claim(&config, &job.id, owner).expect("seed a foreign claim");
            let cleared = cron::clear_stale_locks(&config).expect("recovery should succeed");
            assert_eq!(
                cleared, 1,
                "a foreign claim must be cleared (owner={owner:?})"
            );
            assert!(
                cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
                "the row must be claimable again"
            );
            cron::release_job(&config, &job.id).unwrap();
        }
    }

    #[test]
    fn declarative_jobs_are_withheld_while_reconciliation_is_unresolved() {
        let mut declarative = test_job("echo decl");
        declarative.source = "declarative".into();
        let imperative = test_job("echo imp");

        let jobs = vec![declarative.clone(), imperative.clone()];

        // Reconciliation succeeded: everything runs.
        let kept = withhold_declarative_when_unreconciled(jobs.clone(), false);
        assert_eq!(kept.len(), 2);

        // Reconciliation failed: the declarative row's stored body may predate
        // the config the gate resolves from, so it is held back. Imperative
        // rows have no config declaration to disagree with.
        let kept = withhold_declarative_when_unreconciled(jobs, true);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].source, "imperative");
    }

    #[tokio::test]
    async fn cron_action_budget_persists_across_runs() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        // One action per hour: the first run consumes it, the second must be
        // refused rather than handed a fresh budget.
        config
            .runtime_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .max_actions_per_hour = 1;

        let first = cron_security_policy(&config, TEST_AGENT).expect("policy builds");
        assert!(first.record_action(), "the first action fits the budget");

        let second = cron_security_policy(&config, TEST_AGENT).expect("policy builds");
        assert!(
            !second.record_action(),
            "a later cron run must share the budget, not reset it"
        );
    }

    // ── Ownership resolution for declarative jobs ────────────────────

    #[tokio::test]
    async fn declarative_gate_runs_under_the_current_owner_not_the_stored_one() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;

        // Old owner: allowed to run the hook. New owner: not allowed.
        const NEW_AGENT: &str = "new-owner";
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["exit".into(), "echo".into()];
        config.risk_profiles.insert(
            NEW_AGENT.to_string(),
            zeroclaw_config::schema::RiskProfileConfig {
                allowed_commands: vec!["echo".into()],
                ..Default::default()
            },
        );
        config.runtime_profiles.insert(
            NEW_AGENT.to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig::default(),
        );
        config.providers.models.openrouter.insert(
            NEW_AGENT.to_string(),
            zeroclaw_config::schema::OpenRouterModelProviderConfig::default(),
        );
        config.agents.insert(
            NEW_AGENT.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: format!("openrouter.{NEW_AGENT}").into(),
                risk_profile: NEW_AGENT.into(),
                runtime_profile: NEW_AGENT.into(),
                ..Default::default()
            },
        );

        let job = declarative_gated_job(&mut config, "moved-job", "exit 0", 30);
        // Sync stamped the owner onto the row. That stored alias is exactly
        // what goes stale when config membership later moves.
        assert_eq!(job.agent_alias, TEST_AGENT);

        // Move ownership in live config, exactly as an operator edit would.
        // The stored row is not rewritten, which is the point of the test.
        config
            .agents
            .get_mut(TEST_AGENT)
            .expect("old owner exists")
            .cron_jobs
            .retain(|c| c != "moved-job");
        config
            .agents
            .get_mut(NEW_AGENT)
            .expect("new owner exists")
            .cron_jobs
            .push("moved-job".to_string());

        // The row still carries the old owner; only live config moved.
        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;

        // Under the old owner `exit` is allowed and the gate would pass. Under
        // the new owner it is not, so a correct resolver refuses the hook.
        assert_eq!(
            result.status, STATUS_PRECONDITION_FAILED,
            "the gate must be authorized against the current owner: {}",
            result.output
        );
        assert!(
            result.output.contains("blocked by security policy"),
            "unexpected output: {}",
            result.output
        );
    }

    /// Register a second enabled agent that also claims `cron_alias`.
    fn add_rival_owner(config: &mut Config, alias: &str, cron_alias: &str) {
        config.risk_profiles.insert(
            alias.to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.runtime_profiles.insert(
            alias.to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig::default(),
        );
        config.providers.models.openrouter.insert(
            alias.to_string(),
            zeroclaw_config::schema::OpenRouterModelProviderConfig::default(),
        );
        config.agents.insert(
            alias.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: format!("openrouter.{alias}").into(),
                risk_profile: alias.into(),
                runtime_profile: alias.into(),
                cron_jobs: vec![cron_alias.to_string()],
                ..Default::default()
            },
        );
    }

    #[tokio::test]
    async fn two_enabled_owners_refuse_to_resolve_instead_of_picking_one() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "contested", "exit 0", 30);

        // A second enabled agent now claims the same alias. `Config::agents` is
        // a HashMap, so "first match" would be decided by map order and could
        // change across a restart, silently moving the job (and its
        // config-declared hook) to a different security authority.
        add_rival_owner(&mut config, "rival-owner", "contested");

        let resolved = resolve_owning_agent(&config, &job);
        assert!(
            resolved.is_err(),
            "ambiguous ownership must not resolve to an arbitrary agent, got {resolved:?}"
        );
        let reason = resolved.unwrap_err();
        assert!(
            reason.contains("claimed by 2 enabled agents"),
            "unexpected: {reason}"
        );
        // Both claimants are named so the operator can fix the config.
        assert!(reason.contains(TEST_AGENT) && reason.contains("rival-owner"));

        // And the run refuses rather than executing under a coin-flip policy.
        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;
        assert!(!result.success);
        assert!(
            !body_ran(&result),
            "an unresolved owner must not run the job"
        );
    }

    #[tokio::test]
    async fn a_disabled_rival_owner_does_not_make_ownership_ambiguous() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "one-live-owner", "exit 0", 30);

        add_rival_owner(&mut config, "disabled-rival", "one-live-owner");
        config
            .agents
            .get_mut("disabled-rival")
            .expect("rival exists")
            .enabled = false;

        // Only enabled agents can own a job, so a disabled claimant is not a
        // competing owner and resolution stays determinate.
        assert_eq!(
            resolve_owning_agent(&config, &job).as_deref(),
            Ok(TEST_AGENT)
        );
    }

    #[tokio::test]
    async fn imperative_jobs_still_resolve_through_their_stored_alias() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo owned");

        // Imperative rows carry their owner on the row; live `cron_jobs`
        // membership does not name them, so the stored alias must still win.
        assert_eq!(
            resolve_owning_agent(&config, &job).as_deref(),
            Ok(TEST_AGENT)
        );
    }

    // ── Startup recovery must not mutate a claimed row ───────────────

    #[tokio::test]
    async fn startup_skip_leaves_a_claimed_row_alone() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "startup-race", "exit 0", 30);

        // A manual trigger accepted before the scheduler finished starting.
        assert!(cron::claim_job(&config, &job.id, Utc::now()).unwrap());
        let before = cron::get_job(&config, &job.id).unwrap();

        // Startup recovery then reaches this overdue row.
        cron::skip_missed_run(&config, &before, Utc::now() + ChronoDuration::hours(1))
            .expect("skip should not error on a claimed row");

        let after = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(
            after.next_run, before.next_run,
            "startup skip must not advance a row that another owner is running"
        );
        assert!(after.enabled, "startup skip must not disable a claimed row");
    }

    // ── Manual-run ownership: the claim must cover the gate too ──────

    #[tokio::test]
    async fn manual_run_is_refused_while_the_scheduler_holds_the_claim() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "claimed-job", "exit 0", 30);

        // Stand in for a due scheduled run that already claimed the row.
        assert!(cron::claim_job(&config, &job.id, Utc::now()).unwrap());

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;

        assert!(!result.success);
        assert_eq!(result.status, STATUS_ALREADY_IN_FLIGHT);
        assert!(
            !body_ran(&result),
            "a refused trigger must not run the body"
        );

        // A refusal is not a run: it writes no history and leaves the other
        // owner's claim intact.
        let runs = cron::list_runs(&config, &job.id, 10).expect("run history should list");
        assert!(
            runs.is_empty(),
            "a refused manual trigger must not record a run"
        );
        assert!(
            !cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "the refused trigger must not have released the scheduler's claim"
        );
    }

    #[tokio::test]
    async fn manual_run_releases_its_claim_so_the_next_run_can_take_it() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "release-job", "exit 0", 30);

        let first = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;
        assert_eq!(first.status, "ok");

        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "the claim must be released once the manual run finishes"
        );
    }

    #[tokio::test]
    async fn manual_run_releases_its_claim_even_when_the_gate_refuses_the_run() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        // The gate skips, so the run returns early — the guard still releases.
        let job = declarative_gated_job(&mut config, "skip-release", "exit 10", 30);

        let result = run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None).await;
        assert_eq!(result.status, STATUS_SKIPPED_PRECONDITION);

        assert!(
            cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
            "an early gate return must not strand the claim"
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn two_concurrent_manual_runs_cannot_both_execute() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["exit".into(), "sleep".into()];

        use zeroclaw_config::schema::{CronJobDecl, CronPreHookDecl, CronScheduleDecl};
        config.cron.insert(
            "race-job".to_string(),
            CronJobDecl {
                job_type: "shell".into(),
                schedule: CronScheduleDecl::Cron {
                    expr: "*/5 * * * *".into(),
                    tz: None,
                },
                // A body slow enough that the second trigger is still inside
                // the first one's claim window.
                command: Some("sleep 2".into()),
                pre_hook: Some(CronPreHookDecl {
                    command: "exit 0".into(),
                    timeout_secs: 30,
                }),
                ..CronJobDecl::default()
            },
        );
        config
            .agents
            .get_mut(TEST_AGENT)
            .expect("test agent exists")
            .cron_jobs
            .push("race-job".to_string());
        let decls = config.cron.clone();
        cron::sync_declarative_jobs(&config, &decls).expect("declarative sync should succeed");
        let job = cron::get_job(&config, "race-job").expect("synced job should be readable");

        let (first, second) = tokio::join!(
            run_manual_job(&config, &job, CronDeliveryContext::RpcManual, &None),
            async {
                // Let the first trigger take the claim before the second asks.
                tokio::time::sleep(Duration::from_millis(200)).await;
                run_manual_job(&config, &job, CronDeliveryContext::GatewayManual, &None).await
            }
        );

        assert_eq!(
            first.status, "ok",
            "the first trigger should own the window"
        );
        assert_eq!(
            second.status, STATUS_ALREADY_IN_FLIGHT,
            "the second trigger must be refused, not run in parallel"
        );

        // Exactly one execution reached history.
        let runs = cron::list_runs(&config, &job.id, 10).expect("run history should list");
        assert_eq!(
            runs.len(),
            1,
            "only one of the two triggers may record a run"
        );
    }

    // ── Manual broadcast carries the outcome status ──────────────────

    #[tokio::test]
    async fn manual_broadcast_carries_the_precondition_skip_status() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "skip-broadcast", "exit 10", 30);
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);

        let result =
            run_manual_job(&config, &job, CronDeliveryContext::GatewayManual, &Some(tx)).await;
        assert_eq!(result.status, STATUS_SKIPPED_PRECONDITION);

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("manual trigger should broadcast")
            .expect("broadcast channel should stay open");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["manual"], true);
        // A skip is deliberately `success: true`, so without `status` an SSE
        // consumer could not tell it from an ordinary successful run.
        assert_eq!(event["success"], true);
        assert_eq!(event["status"], STATUS_SKIPPED_PRECONDITION);
    }

    #[tokio::test]
    async fn manual_broadcast_carries_the_precondition_failure_status() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        allow_gate_test_commands(&mut config);
        let job = declarative_gated_job(&mut config, "fail-broadcast", "exit 3", 30);
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);

        let result =
            run_manual_job(&config, &job, CronDeliveryContext::GatewayManual, &Some(tx)).await;
        assert_eq!(result.status, STATUS_PRECONDITION_FAILED);

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("manual trigger should broadcast")
            .expect("broadcast channel should stay open");
        assert_eq!(event["success"], false);
        assert_eq!(event["status"], STATUS_PRECONDITION_FAILED);
    }

    #[test]
    fn cron_run_outcome_maps_each_class_to_its_own_status() {
        assert_eq!(
            CronRunOutcome::executed(true, String::new()).base_status(),
            "ok"
        );
        assert_eq!(
            CronRunOutcome::executed(false, String::new()).base_status(),
            "error"
        );
        assert_eq!(
            CronRunOutcome::SkippedByPrecondition {
                output: String::new()
            }
            .base_status(),
            STATUS_SKIPPED_PRECONDITION
        );
        assert_eq!(
            CronRunOutcome::PreconditionFailed {
                output: String::new()
            }
            .base_status(),
            STATUS_PRECONDITION_FAILED
        );
        // All four statuses are distinct, which is the whole point of the enum.
        let statuses = [
            CronRunOutcome::executed(true, String::new()).base_status(),
            CronRunOutcome::executed(false, String::new()).base_status(),
            STATUS_SKIPPED_PRECONDITION,
            STATUS_PRECONDITION_FAILED,
        ];
        let unique: std::collections::HashSet<&str> = statuses.iter().copied().collect();
        assert_eq!(unique.len(), statuses.len());
    }
}
