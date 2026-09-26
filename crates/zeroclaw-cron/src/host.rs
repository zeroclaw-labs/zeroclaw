//! Host capabilities consumed by the cron scheduler.
//!
//! Cron owns scheduling, policy admission, and the effective policy for a run.
//! The embedding runtime owns agent execution and process health reporting, so
//! it supplies those capabilities explicitly when it starts or manually drives
//! the scheduler.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

/// Outcome of running one agent-backed cron job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronAgentRun {
    /// Whether the run is reported as successful.
    pub success: bool,
    /// Operator-facing output, already bounded by the executor.
    pub output: String,
}

/// Everything the host needs to execute an admitted agent job.
///
/// `config` and `security` are the snapshots resolved by cron for this run.
/// Passing the effective policy directly keeps it authoritative across the
/// crate boundary; the host must not rebuild it from partial inputs.
pub struct CronAgentRequest {
    /// Configuration snapshot used to admit and execute the run.
    pub config: Config,
    /// Effective security policy after cron-specific narrowing.
    pub security: Arc<SecurityPolicy>,
    /// Stable id of the job being run.
    pub job_id: String,
    /// Alias of the agent executing the run.
    pub agent_alias: String,
    /// The prompt to run.
    pub prompt: String,
    /// Optional model override.
    pub model: Option<String>,
    /// Session path the run should use: `main` or an isolated per-run path.
    pub session_path: std::path::PathBuf,
    /// Optional per-run tool allowlist.
    pub allowed_tools: Option<Vec<String>>,
    /// Whether memory context is recalled and injected for this run.
    pub uses_memory: bool,
}

/// Runs the agent side of a cron job.
///
/// The trait lives with its consumer. A host implements it and passes the
/// implementation into cron explicitly; no process-global registration is
/// required.
pub trait CronAgentExecutor: Send + Sync {
    /// Execute one admitted agent job and report its outcome.
    fn run_agent_job<'a>(
        &'a self,
        request: CronAgentRequest,
    ) -> Pin<Box<dyn Future<Output = CronAgentRun> + Send + 'a>>;
}

/// Reports scheduler liveness to the host's process health registry.
pub trait CronHealthReporter: Send + Sync {
    /// Record that `component` is functioning.
    fn mark_ok(&self, component: &str);
    /// Record that `component` has failed, with an operator-facing reason.
    fn mark_error(&self, component: &str, reason: &str);
}

/// A health reporter for embeddings that do not expose a health registry.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopCronHealth;

impl CronHealthReporter for NoopCronHealth {
    fn mark_ok(&self, _component: &str) {}
    fn mark_error(&self, _component: &str, _reason: &str) {}
}
