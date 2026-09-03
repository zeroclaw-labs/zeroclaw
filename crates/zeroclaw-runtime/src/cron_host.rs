//! Runtime-side implementations of the cron seams.
//!
//! `zeroclaw-cron` decides *when* a job runs and *whether* policy admits it.
//! Running the agent and reporting process health belong to the runtime, and
//! the runtime starts the scheduler, so cron reaches back through traits in
//! `zeroclaw-api` rather than depending on this crate. These are the
//! implementations it reaches.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use zeroclaw_api::cron_traits::{
    CronAgentExecutor, CronAgentRequest, CronAgentRun, CronHealthReporter,
};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;
use zeroclaw_log::Instrument;

/// Bridges cron's health calls onto the runtime's component registry.
pub struct RuntimeCronHealth;

impl CronHealthReporter for RuntimeCronHealth {
    fn mark_ok(&self, component: &str) {
        crate::health::mark_component_ok(component);
    }

    fn mark_error(&self, component: &str, reason: &str) {
        crate::health::mark_component_error(component, reason.to_string());
    }
}

/// Runs cron's agent jobs through the agent loop.
///
/// Holds the config the daemon was started with. Cron passes the job-specific
/// parts of each run; everything else about how an agent executes is this
/// crate's business and stays here.
pub struct RuntimeCronAgentExecutor {
    config: Config,
}

impl RuntimeCronAgentExecutor {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Build the effective policy for one cron agent run.
    ///
    /// The request carries `excluded_tools` rather than a policy because
    /// `zeroclaw-api` cannot see `zeroclaw-config` types. Cron decided what to
    /// narrow; applying it is this side's job, and skipping it would widen the
    /// run past what cron admitted.
    fn run_policy(&self, request: &CronAgentRequest) -> anyhow::Result<SecurityPolicy> {
        let mut policy = SecurityPolicy::for_agent(&self.config, &request.agent_alias)?;
        // The scheduler may run a job in a workspace that is not the agent's
        // default. Rebuilding from the alias alone would quietly substitute
        // the default and undo that choice.
        policy.workspace_dir = request.workspace_dir.clone();
        if !request.excluded_tools.is_empty() {
            let excluded = policy.excluded_tools.get_or_insert_with(Vec::new);
            for tool in &request.excluded_tools {
                if !excluded.iter().any(|existing| existing == tool) {
                    excluded.push(tool.clone());
                }
            }
        }
        Ok(policy)
    }
}

impl CronAgentExecutor for RuntimeCronAgentExecutor {
    fn run_agent_job<'a>(
        &'a self,
        request: CronAgentRequest,
    ) -> Pin<Box<dyn Future<Output = CronAgentRun> + Send + 'a>> {
        Box::pin(async move {
            let run_security = match self.run_policy(&request) {
                Ok(policy) => policy,
                Err(e) => {
                    return CronAgentRun {
                        success: false,
                        output: format!("agent job failed: {e}"),
                    };
                }
            };

            // Cron jobs never auto-save conversation memory: a scheduled run is
            // not a conversation, and letting it write would accumulate turns
            // nobody asked for.
            let mut cron_config = self.config.clone();
            cron_config.memory.auto_save = false;

            let span = zeroclaw_log::info_span!(
                "subagent",
                category = "cron",
                agent_alias = %request.agent_alias,
                cron_job_id = %request.job_id,
                spawn_site = "cron",
            );

            let overrides = crate::agent::loop_::AgentRunOverrides {
                security: Some(Arc::new(run_security)),
                memory: None,
                is_subagent: false,
                // `uses_memory = false` opts the job out of memory-context
                // injection and makes the run memory-free end to end: the loop
                // binds a `NoneMemory` backend and drops the persistent memory
                // tools, so such a job can neither recall/store through a real
                // backend nor reach one via advertised tools.
                suppress_memory_inject: !request.uses_memory,
                memory_free: !request.uses_memory,
                // Cron runs are short-lived and one-shot, so the per-call
                // `connect_all` path inside `agent::run` is correct here. The
                // daemon heartbeat worker is the only `mcp_registry` supplier.
                mcp_registry: None,
            };

            let temperature = self
                .config
                .model_provider_for_agent(&request.agent_alias)
                .and_then(|e| e.temperature);

            let result = Box::pin(
                crate::agent::run(
                    cron_config,
                    &request.agent_alias,
                    Some(request.prompt.clone()),
                    None,
                    request.model.clone(),
                    temperature,
                    vec![],
                    false,
                    Some(request.session_path.clone()),
                    request.allowed_tools.clone(),
                    zeroclaw_api::ingress::TurnOrigin::Cron,
                    overrides,
                )
                .instrument(span),
            )
            .await;

            match result {
                Ok(response) => CronAgentRun {
                    success: true,
                    output: if response.trim().is_empty() {
                        "agent job executed".to_string()
                    } else {
                        response
                    },
                },
                Err(e) => {
                    // A failed isolated run leaves session memory behind that
                    // nothing will ever read. Purge it rather than accumulate
                    // one dead session per failure.
                    if request.session_path != std::path::Path::new("main") {
                        let key = zeroclaw_api::session_keys::sanitize_session_key(&format!(
                            "cli:{}",
                            request.session_path.display()
                        ));
                        if let Ok(mem) = zeroclaw_memory::create_memory_for_agent(
                            &self.config,
                            &request.agent_alias,
                            self.config
                                .model_provider_for_agent(&request.agent_alias)
                                .and_then(|e| e.api_key.as_deref()),
                        )
                        .await
                        {
                            let _ = mem.purge_session(&key).await;
                        }
                    }
                    CronAgentRun {
                        success: false,
                        output: format!("agent job failed: {e}"),
                    }
                }
            }
        })
    }
}

/// Register both cron seams with the scheduler.
///
/// Call once before starting the scheduler. Registration is first-wins, so a
/// second call is a no-op rather than a surprise swap mid-run.
pub fn register_cron_host(config: Config) {
    zeroclaw_cron::scheduler::register_health_reporter(Arc::new(RuntimeCronHealth));
    zeroclaw_cron::scheduler::register_agent_executor(Arc::new(RuntimeCronAgentExecutor::new(
        config,
    )));
}
