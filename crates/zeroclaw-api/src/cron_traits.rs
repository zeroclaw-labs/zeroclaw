//! Seams that let cron scheduling live outside the agent runtime.
//!
//! Cron owns *when* work runs and *whether* it is allowed to run. It does not
//! own agent execution or the process health registry. Those live in the
//! runtime, and the runtime starts the scheduler, so cron cannot depend on the
//! runtime crate without a cycle. It depends on these traits instead, and the
//! runtime supplies the implementations at scheduler construction.

use std::future::Future;
use std::pin::Pin;

/// Outcome of running one agent-backed cron job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronAgentRun {
    /// Whether the run is reported as successful.
    pub success: bool,
    /// Operator-facing output, already bounded by the executor.
    pub output: String,
}

/// Everything cron knows about an agent job it is asking someone else to run.
///
/// Deliberately not the cron row type: cron owns that shape and is free to
/// change it, while this is a contract with the runtime and should move only
/// when the contract genuinely changes.
#[derive(Debug, Clone)]
pub struct CronAgentRequest {
    /// Stable id of the job being run.
    pub job_id: String,
    /// Alias of the agent whose policy authorizes and executes the run.
    pub agent_alias: String,
    /// The prompt to run.
    pub prompt: String,
    /// Optional model override.
    pub model: Option<String>,
    /// Session path the run should use: `main` or an isolated per-run path.
    pub session_path: std::path::PathBuf,
    /// Optional per-run tool allowlist.
    pub allowed_tools: Option<Vec<String>>,
    /// Workspace the scheduler resolved for this run.
    ///
    /// The scheduler's effective policy can carry a workspace that differs
    /// from the agent's default, and a run has to execute in that one. The
    /// host rebuilds the policy from the alias, so without this the rebuild
    /// would silently substitute the agent default and undo the scheduler's
    /// choice.
    pub workspace_dir: std::path::PathBuf,
    /// Tools cron requires the host to exclude from this run.
    ///
    /// Cron narrows scheduler-mutation tools out of its own agent jobs so a
    /// scheduled run cannot rewrite the schedule that started it. The host
    /// must apply these on top of whatever the agent's profile allows; a host
    /// that ignores them widens the run beyond what cron admitted.
    pub excluded_tools: Vec<String>,
    /// Whether memory context is recalled and injected for this run.
    pub uses_memory: bool,
}

/// Runs the agent side of a cron job.
///
/// Implemented by the runtime, consumed by the scheduler. Keeping this async
/// through a boxed future rather than `async_trait` keeps `zeroclaw-api` free
/// of that dependency, matching the other trait seams in this crate.
pub trait CronAgentExecutor: Send + Sync {
    /// Execute one agent job and report its outcome.
    ///
    /// Implementations must not panic: cron treats a returned failure as a job
    /// failure, and has no way to recover from an unwound executor.
    fn run_agent_job<'a>(
        &'a self,
        request: CronAgentRequest,
    ) -> Pin<Box<dyn Future<Output = CronAgentRun> + Send + 'a>>;
}

/// Reports scheduler liveness to the process health registry.
///
/// Cron marks itself healthy on every completed poll, including idle ones, so
/// a silent scheduler is distinguishable from an idle one. The registry itself
/// belongs to the runtime.
pub trait CronHealthReporter: Send + Sync {
    /// Record that `component` is functioning.
    fn mark_ok(&self, component: &str);
    /// Record that `component` has failed, with an operator-facing reason.
    fn mark_error(&self, component: &str, reason: &str);
}

/// A health reporter that discards everything.
///
/// For tests and for embeddings that do not run the runtime's health registry.
/// Named rather than anonymous so a caller opting out of health reporting has
/// to say so.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopCronHealth;

impl CronHealthReporter for NoopCronHealth {
    fn mark_ok(&self, _component: &str) {}
    fn mark_error(&self, _component: &str, _reason: &str) {}
}
