//! Agent-loop tool that spawns an ephemeral SubAgent inheriting the
//! parent's identity, security policy, and memory allowlist, runs a
//! focused prompt, and returns the response. Cron's `JobType::Agent`
//! dispatch is the other SubAgent spawn site; both funnel through

use crate::agent::loop_::AgentRunOverrides;
use crate::live_config_authority::AgentExecutionCapability;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use crate::subagent::{SubAgentOverrides, SubAgentSpawn};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::FutureExt;
use serde_json::json;
use std::future::Future;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::Config;
use zeroclaw_log::scope;

/// Spawn an ephemeral SubAgent that inherits the parent agent's
/// identity and runs a focused prompt under the same alias.
pub struct SpawnSubagentTool {
    config: Arc<Config>,
    parent_alias: String,
    security: Arc<SecurityPolicy>,
    /// `true` when this tool is registered inside a run that is itself
    /// a SubAgent. Triggers a depth-1 cap refusal in `execute` before
    /// any spawn work happens. Set by the agent loop from
    /// `AgentRunOverrides.is_subagent` at registry construction time.
    is_subagent_caller: bool,
    execution_capability: Option<AgentExecutionCapability>,
}

impl SpawnSubagentTool {
    /// Canonical tool name. Referenced by `REENTRANT_AGENT_TOOLS` so a
    /// rename cannot desync the two.
    pub const NAME: &'static str = "spawn_subagent";

    pub fn new(
        config: Arc<Config>,
        parent_alias: impl Into<String>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            config,
            parent_alias: parent_alias.into(),
            security,
            is_subagent_caller: false,
            execution_capability: None,
        }
    }

    /// Mark this tool instance as belonging to a SubAgent's tool
    /// registry. Triggers the depth-1 refusal on `execute`. The agent
    /// loop sets this from `AgentRunOverrides.is_subagent`.
    #[must_use]
    pub fn with_subagent_caller(mut self, is_subagent_caller: bool) -> Self {
        self.is_subagent_caller = is_subagent_caller;
        self
    }

    pub fn with_execution_capability(
        mut self,
        capability: Option<AgentExecutionCapability>,
    ) -> Self {
        self.execution_capability = capability;
        self
    }
}

/// Overrides for the child run this tool starts.
///
/// The child inherits the parent's security policy and, when the parent turn is
/// a headless SOP step, that step's tool scope. Spawning a child is not a way
/// out of the step's capability boundary: without this the child would rebuild
/// the agent's full surface, including tools the step denies and the SOP control
/// tools every step turn drops. The scope is carried, not resolved here — the
/// child re-resolves it against its own registry, so a differently assembled
/// child registry is narrowed by the same contract.
fn child_run_overrides(policy: Arc<SecurityPolicy>) -> AgentRunOverrides {
    AgentRunOverrides {
        security: Some(policy),
        memory: None,
        is_subagent: true,
        // Sub-turn origin already skips memory injection; explicit for
        // the same future-proofing reason as `is_subagent` above.
        suppress_memory_inject: true,
        // Subagents keep a live memory backend and the memory tools; only
        // the injected context preamble is suppressed above.
        memory_free: false,
        suppress_memory_auto_save: false,
        // Subagent runs are short-lived; no cross-turn reuse contract,
        // so the per-call `connect_all` path inside `agent::run` is
        // the correct choice. The daemon heartbeat worker is the
        // only `mcp_registry` supplier.
        mcp_registry: None,
        execution_capability: None,
        execution_admission: None,
        sop_step_scope: crate::sop::active_scope::active_headless_step_scope(),
        // Not yet propagated: a sub-turn spawned by an internally
        // initiated parent (e.g. a cron turn delegating) loses the
        // parent's initiating principal here. Inheritance semantics for
        // nested dispatch belong to the reply-provenance slice of the
        // internal-turn contract; until then absence is explicit.
        internal_principal: None,
    }
}

async fn settle_goal_subagent_row(
    cp: &crate::control_plane::ControlPlaneHandle,
    task_id: &str,
    status: crate::control_plane::TaskStatus,
    output: Option<String>,
    error: Option<String>,
) -> Result<()> {
    let won = cp
        .store
        .transition_terminal_if_owner(
            task_id,
            std::process::id(),
            &cp.boot_id,
            status,
            output,
            error,
        )
        .await?;
    if !won {
        let snapshot = cp.store.get_snapshot(task_id).await?;
        anyhow::ensure!(
            snapshot.is_some_and(|snapshot| snapshot.task.status.is_terminal()),
            "Goal subagent terminal transition lost without a terminal winner"
        );
    }
    Ok(())
}

async fn run_goal_spawn_owned<F>(
    context: &crate::goal_accounting::GoalForegroundContext,
    task_id: &str,
    run_future: F,
) -> Result<ToolResult>
where
    F: Future<Output = Result<String>> + Send,
{
    let mut caught = Box::pin(std::panic::AssertUnwindSafe(run_future).catch_unwind());
    // The poll adaptor and drain selector borrow this flag across awaits;
    // their shared reference must remain Send in the Tool trait future.
    let body_started = std::sync::atomic::AtomicBool::new(false);
    let mut body = Box::pin(std::future::poll_fn(|cx| {
        body_started.store(true, std::sync::atomic::Ordering::Relaxed);
        caught.as_mut().poll(cx)
    }));
    let mut stop = None;
    let result = tokio::select! {
        biased;
        () = context.token().cancelled() => {
            stop = Some(crate::control_plane::TaskStatus::Cancelled);
            context.token().cancel();
            if body_started.load(std::sync::atomic::Ordering::Relaxed) { body.await } else { Ok(Err(zeroclaw_api::tool::ToolExecutionCancelled.into())) }
        }
        () = async {
            match context.deadline() {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        } => {
            stop = Some(crate::control_plane::TaskStatus::TimedOut);
            context.token().cancel();
            if body_started.load(std::sync::atomic::Ordering::Relaxed) { body.await } else { Ok(Err(zeroclaw_api::tool::ToolExecutionCancelled.into())) }
        }
        result = &mut body => result,
    };
    let status = stop.unwrap_or(match &result {
        Ok(Ok(_)) => crate::control_plane::TaskStatus::Completed,
        _ => crate::control_plane::TaskStatus::Failed,
    });
    let cp = context.control_plane();
    let (output, error) = match (&result, status) {
        (Ok(Ok(response)), crate::control_plane::TaskStatus::Completed) => {
            (Some(response.clone()), None)
        }
        (Ok(Err(error)), crate::control_plane::TaskStatus::Failed) => (
            None,
            Some(crate::security::scrub(&format!(
                "subagent run failed: {error}"
            ))),
        ),
        _ => (None, None),
    };
    let cleanup = settle_goal_subagent_row(cp, task_id, status, output, error).await;
    let run_result = match result {
        Ok(result) => result,
        Err(panic) => {
            if let Err(error) = cleanup {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(serde_json::json!({
                            "task_id": task_id,
                            "error": crate::security::scrub(&format!("{error:#}")),
                        })),
                    "Goal subagent panic cleanup failed"
                );
            }
            std::panic::resume_unwind(panic);
        }
    };
    let run_result = match (run_result, cleanup) {
        (Err(error), Err(cleanup)) => {
            let error = error.context(format!("Goal subagent cleanup failed: {cleanup:#}"));
            Err(
                if zeroclaw_api::turn_stop::turn_stop(&error).is_some_and(|stop| {
                    stop.code == zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
                }) {
                    crate::agent::turn::wrap_goal_spend_refusal(error, false)
                } else {
                    error
                },
            )
        }
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (result, Ok(())) => result,
    };
    if let Some(status) = stop {
        let error = run_result
            .err()
            .unwrap_or_else(|| anyhow::Error::new(zeroclaw_api::tool::ToolExecutionCancelled));
        return Err(if status == crate::control_plane::TaskStatus::TimedOut {
            error.context(zeroclaw_api::turn_stop::TurnStop::recoverable(
                zeroclaw_api::turn_stop::TurnStopCode::TurnTimeout,
                "Goal subagent timed out",
            ))
        } else {
            error
        });
    }
    run_result.map(|response| ToolResult {
        success: true,
        output: if response.trim().is_empty() {
            "subagent completed without output".to_string().into()
        } else {
            response.into()
        },
        error: None,
    })
}

#[async_trait]
impl Tool for SpawnSubagentTool {
    fn may_invoke_agent_child(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn execute_with_context(
        &self,
        args: serde_json::Value,
        context: &zeroclaw_api::tool::ToolExecutionContext,
    ) -> Result<ToolResult> {
        if context.is_goal_foreground()
            || crate::goal_accounting::GoalForegroundContext::is_participating()
        {
            context.scope_only(self.execute(args)).await
        } else {
            context.run(self.execute(args)).await
        }
    }
    fn requires_unrestricted_principal(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Spawn an ephemeral SubAgent that inherits this agent's identity, \
         security policy, and memory allowlist. The SubAgent runs the supplied \
         prompt to completion under the parent's permissions envelope and \
         returns its response. Use for focused subtasks (research lookup, \
         multi-step reasoning, etc.) that should not pollute this agent's main \
         conversation history. Cost-aware: each SubAgent run is a full agent \
         loop and consumes provider tokens."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task or question for the SubAgent. Be specific and self-contained — the SubAgent does not see this conversation's history."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let goal = crate::goal_accounting::GoalForegroundContext::current_for_child()
            .map_err(|error| error.context(zeroclaw_api::tool::GoalChildOperationError))?;
        if let Some(parent) = goal {
            if parent.depth() >= 1 {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(crate::i18n::get_required_cli_string(
                        "goal-mode-child-recursion-disallowed",
                    )),
                });
            }
            if parent.token().is_cancelled()
                || parent
                    .deadline()
                    .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
            {
                return Err(
                    anyhow::Error::new(zeroclaw_api::tool::ToolExecutionCancelled)
                        .context(zeroclaw_api::tool::GoalChildOperationError),
                );
            }
            let child = parent.child();
            return child
                .scope(self.execute_inner(args, Some(&child)))
                .await
                .map_err(|error| error.context(zeroclaw_api::tool::GoalChildOperationError));
        }
        self.execute_inner(args, None).await
    }
}

impl SpawnSubagentTool {
    async fn execute_inner(
        &self,
        args: serde_json::Value,
        goal: Option<&crate::goal_accounting::GoalForegroundContext>,
    ) -> Result<ToolResult> {
        // Depth-1 cap: a SubAgent may not spawn its own subagents.
        // The caller-side flag is set at registry construction time
        // from `AgentRunOverrides.is_subagent`, so the refusal fires
        // before any spawn work and before the risk_profile gate.
        if self.is_subagent_caller {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    "spawn_subagent: a subagent may not spawn its own subagents (depth-1 cap)"
                        .into(),
                ),
            });
        }

        let execution_admission = match self
            .execution_capability
            .as_ref()
            .map(|capability| capability.admit(&self.parent_alias))
            .transpose()
        {
            Ok(admission) => admission,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("subagent admission failed: {error}")),
                });
            }
        };
        let config = execution_admission
            .as_ref()
            .map(|admission| admission.config())
            .unwrap_or_else(|| Arc::clone(&self.config));
        let risk_profile = config.risk_profile_for_agent(&self.parent_alias);
        if let Some(rp) = risk_profile {
            let excluded = rp.excluded_tools.iter().any(|t| t == "spawn_subagent");
            let allowed_when_listed = rp
                .effective_allowed_tools()
                .is_none_or(|tools| tools.iter().any(|t| t == "spawn_subagent"));
            if excluded || !allowed_when_listed {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "spawn_subagent: refused — agent '{}' risk_profile does not list spawn_subagent in allowed_tools",
                        self.parent_alias
                    )),
                });
            }
        }

        // Argument validation surfaces as a structured `ToolResult`
        // failure (matching the unknown-parent and run-failure shapes
        // below) so the agent loop receives a uniform "tool reported
        // failure" signal regardless of which step rejected the call.
        let prompt = match args
            .get("prompt")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(p) => p.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing or empty 'prompt' parameter".into()),
                });
            }
        };

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, Self::NAME)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let subagent_ctx = match SubAgentSpawn::for_agent_with_policy(
            &config,
            &self.parent_alias,
            Arc::clone(&self.security),
        )
        .and_then(|spawn| spawn.build(SubAgentOverrides::default()))
        {
            Ok(ctx) => ctx,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("subagent spawn failed: {e:#}")),
                });
            }
        };

        let run_id = uuid::Uuid::new_v4().to_string();

        let temperature: Option<f64> = config
            .model_provider_for_agent(&self.parent_alias)
            .and_then(|e| e.temperature);
        let session_path = std::path::PathBuf::from(format!("subagent-{run_id}"));

        let mut run_overrides = child_run_overrides(subagent_ctx.policy.clone());
        run_overrides.execution_admission = execution_admission.clone();
        let parent_alias = subagent_ctx.parent_alias.clone();

        let cp_task_id = run_id.clone();
        let cp = goal
            .map(|context| context.control_plane().clone())
            .or_else(|| crate::control_plane::control_plane().cloned());
        if let Some(cp) = cp.as_ref() {
            let created = cp
                .store
                .create(crate::control_plane::TaskRecord {
                    id: cp_task_id.clone(),
                    kind: crate::control_plane::TaskKind::Subagent,
                    agent: self.parent_alias.clone(),
                    status: crate::control_plane::TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: cp.boot_id.clone(),
                    heartbeat_at: None,
                    depth: goal.map_or(u32::from(self.is_subagent_caller), |context| {
                        context.depth()
                    }),
                    parent_id: goal.map(|context| context.scope_identity().task_id().to_string()),
                    originator_route: None,
                    originator_chain: Vec::new(),
                    delivered: false,
                    idem_key: None,
                    principal_id: None,
                    session_id: None,
                    execution_epoch: 0,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                })
                .await;
            if goal.is_some() {
                created?;
            }
        }

        let run_future = Box::pin(scope!(
            agent_alias: parent_alias,
            session_key: run_id,
            =>
            crate::agent::run(
                (*config).clone(),
                &self.parent_alias,
                Some(prompt),
                None,
                None,
                temperature,
                vec![],
                false,
                Some(session_path),
                None,
                zeroclaw_api::ingress::TurnOrigin::SubTurn,
                run_overrides,
            )
        ));

        if let Some(context) = goal {
            return run_goal_spawn_owned(context, &cp_task_id, run_future).await;
        }
        let run_result = run_future.await;

        // EPIC-A supervision: mirror the subagent's terminal state into the control-plane.
        if let Some(cp) = crate::control_plane::control_plane() {
            let (status, output, error) = match &run_result {
                Ok(resp) => (
                    crate::control_plane::TaskStatus::Completed,
                    Some(resp.clone()),
                    None,
                ),
                Err(e) => (
                    crate::control_plane::TaskStatus::Failed,
                    None,
                    Some(format!("subagent run failed: {e}")),
                ),
            };
            let _ = cp
                .store
                .update_status(&cp_task_id, status, output, error)
                .await;
        }

        match run_result {
            Ok(response) => Ok(ToolResult {
                success: true,
                output: if response.trim().is_empty() {
                    "subagent completed without output".to_string().into()
                } else {
                    response.into()
                },
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("subagent run failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEANUP_CHILD: &str = "goal-cleanup-child";
    const CLEANUP_SQL_ERROR: &str = "fixture C8 exact child terminal SQL abort";

    async fn goal_spawn_cleanup_fixture() -> (
        tempfile::TempDir,
        crate::control_plane::ControlPlaneHandle,
        crate::goal_accounting::GoalForegroundContext,
    ) {
        use crate::control_plane::{ControlPlaneHandle, TaskKind, TaskRecord, TaskStatus};
        let directory = tempfile::TempDir::new().unwrap();
        let cp = ControlPlaneHandle::open(directory.path()).unwrap();
        let scope = zeroclaw_goal::GoalExecutionScope::new(
            "goal-cleanup-parent",
            "goal-cleanup-session",
            1,
        )
        .unwrap();
        let parent = TaskRecord {
            id: scope.task_id().into(),
            kind: TaskKind::Goal,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: std::process::id(),
            owner_boot_id: cp.boot_id.clone(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: Some(scope.session_id().into()),
            execution_epoch: 1,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
        };
        let child = TaskRecord {
            id: CLEANUP_CHILD.into(),
            kind: TaskKind::Subagent,
            depth: 1,
            parent_id: Some(scope.task_id().into()),
            session_id: None,
            execution_epoch: 0,
            ..parent.clone()
        };
        assert_eq!(
            cp.goal_store()
                .unwrap()
                .create_or_replace_session_goal(
                    parent,
                    zeroclaw_goal::GoalTaskRecord {
                        task_id: scope.task_id().into(),
                        objective: "owned cleanup fixture".into(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap(),
            zeroclaw_goal::GoalTransitionResult::Applied
        );
        cp.store.create(child).await.unwrap();
        let context = crate::goal_accounting::GoalForegroundContext::new_root(
            scope,
            cp.clone(),
            tokio_util::sync::CancellationToken::new(),
            Some(tokio::time::Instant::now() + std::time::Duration::from_secs(60)),
        )
        .child();
        (directory, cp, context)
    }

    fn fail_exact_child_terminal_write(directory: &std::path::Path) {
        // A regular trigger on this test's temporary database affects the real
        // store connection; SQLite TEMP triggers would affect only this handle.
        rusqlite::Connection::open(directory.join("control_plane.db"))
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_goal_child_terminal BEFORE UPDATE OF status ON tasks
                WHEN OLD.id = 'goal-cleanup-child' AND OLD.kind = 'subagent'
                  AND NEW.status IN ('completed', 'failed', 'cancelled', 'lost', 'timed_out')
                BEGIN SELECT RAISE(ABORT, 'fixture C8 exact child terminal SQL abort'); END;",
            )
            .unwrap();
    }

    #[derive(Default)]
    struct RefusedSpawnEffects {
        provider: std::sync::atomic::AtomicUsize,
        admit: std::sync::atomic::AtomicUsize,
        settle: std::sync::atomic::AtomicUsize,
    }

    struct RefusedSpawnProvider(Arc<RefusedSpawnEffects>);

    impl zeroclaw_api::attribution::Attributable for RefusedSpawnProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "refused-spawn-provider"
        }
    }

    #[async_trait]
    impl zeroclaw_providers::ModelProvider for RefusedSpawnProvider {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> Result<String> {
            self.0
                .provider
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("refused spawn unexpectedly dispatched a provider request")
        }
    }

    struct RefusedSpawnAccounting(Arc<RefusedSpawnEffects>);

    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for RefusedSpawnAccounting {
        async fn admit(&self, _: zeroclaw_goal::accounting::GoalOperationRequest) -> Result<()> {
            self.0
                .admit
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("refused spawn unexpectedly admitted a Goal operation")
        }
        async fn settle(
            &self,
            _: zeroclaw_goal::accounting::GoalOperationSettlement,
        ) -> Result<()> {
            self.0
                .settle
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("refused spawn unexpectedly settled a Goal operation")
        }
        async fn pricing_complete(&self) -> Result<bool> {
            anyhow::bail!("pre-admission refusal unexpectedly queried pricing completeness")
        }
    }

    async fn actual_checked_spawn_refusal(directory: &std::path::Path) -> anyhow::Error {
        use crate::agent::cost::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
        use zeroclaw_api::model_provider::{ChatMessage, ChatRequest};
        let tracker = Arc::new(
            crate::cost::CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: true,
                    daily_limit_usd: 1.0,
                    monthly_limit_usd: 100.0,
                    ..Default::default()
                },
                directory,
            )
            .unwrap(),
        );
        tracker
            .record_usage(crate::cost::types::TokenUsage::new(
                "ordinary-seed",
                1_000_000,
                0,
                0,
                2.0,
                0.0,
                0.0,
            ))
            .unwrap();
        let ledger = directory.join("state/costs.jsonl");
        let before = std::fs::read(&ledger).unwrap();
        let effects = Arc::new(RefusedSpawnEffects::default());
        let provider = RefusedSpawnProvider(effects.clone());
        let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(RefusedSpawnAccounting(effects.clone()));
        let mut summaries = Vec::new();
        let messages = [ChatMessage::user("refuse before the child request")];
        let error = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(
                Some(accountant),
                TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                    Some(ToolLoopCostTrackingContext::new(
                        tracker,
                        Arc::new(crate::agent::cost::ModelProviderPricing::new()),
                    )),
                    async {
                        crate::agent::turn::ResolvedModelAccess {
                            model_provider: &provider,
                            provider_name: "custom",
                            model: "test-model",
                            dispatch_model: "test-model",
                            temperature: None,
                        }
                        .run_model_query(
                            ChatRequest {
                                messages: &messages,
                                tools: None,
                                thinking: None,
                            },
                            &mut summaries,
                        )
                        .await
                        .unwrap_err()
                    },
                ),
            )
            .await;
        assert_eq!(
            effects.provider.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(effects.admit.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(effects.settle.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(summaries.is_empty());
        assert_eq!(
            std::fs::read(&ledger).unwrap(),
            before,
            "refusal writes no consumption"
        );
        assert_eq!(spawn_refusal_proofs(&error), vec![true]);
        error
    }

    fn spawn_refusal_proofs(error: &anyhow::Error) -> Vec<bool> {
        error
            .chain()
            .filter_map(|source| {
                source.downcast_ref::<zeroclaw_goal::accounting::GoalOrdinarySpendRefusal>()
            })
            .map(zeroclaw_goal::accounting::GoalOrdinarySpendRefusal::clean_tool_phase)
            .collect()
    }

    #[tokio::test]
    async fn goal_settlement_spawn_refusal_cleanup_sql_abort_retains_row_and_negative_source() {
        use crate::control_plane::{TaskKind, TaskStatus};
        use zeroclaw_api::turn_stop::{TurnStop, TurnStopCode, turn_stop};
        for fault in [false, true] {
            let (directory, cp, context) = goal_spawn_cleanup_fixture().await;
            if fault {
                fail_exact_child_terminal_write(directory.path());
            }
            let error = context
                .scope(run_goal_spawn_owned(&context, CLEANUP_CHILD, async {
                    Err(actual_checked_spawn_refusal(directory.path()).await)
                }))
                .await
                .unwrap_err();
            assert_eq!(
                turn_stop(&error).unwrap().code,
                TurnStopCode::BudgetExhausted
            );
            assert_eq!(
                spawn_refusal_proofs(&error),
                if fault { vec![false, true] } else { vec![true] }
            );
            let row = cp.store.get_snapshot(CLEANUP_CHILD).await.unwrap().unwrap();
            assert_eq!(row.task.kind, TaskKind::Subagent);
            assert_eq!(
                row.task.status,
                if fault {
                    TaskStatus::Running
                } else {
                    TaskStatus::Failed
                }
            );
            assert_eq!(row.task.owner_pid, std::process::id());
            assert_eq!(row.task.owner_boot_id, cp.boot_id);
            assert_eq!(row.task.parent_id.as_deref(), Some("goal-cleanup-parent"));
            assert_eq!(row.task.depth, 1);
            assert!(row.output.is_none());
            if fault {
                let diagnostic = format!("{error:#}");
                assert!(
                    diagnostic.contains(CLEANUP_SQL_ERROR),
                    "the exact terminal trigger must fire: {diagnostic}"
                );
                assert!(diagnostic.contains("transition owner-checked task terminal"));
                assert_eq!(
                    diagnostic.matches("Goal subagent cleanup failed:").count(),
                    1
                );
                assert!(row.task.finished_at.is_none());
                assert!(row.error.is_none());
            } else {
                assert!(row.task.finished_at.is_some());
                assert!(!format!("{error:#}").contains("Goal subagent cleanup failed:"));
            }
            let goal = cp
                .goal_store()
                .unwrap()
                .get_goal_task("goal-cleanup-parent")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                goal.accounting_state,
                zeroclaw_goal::GoalAccountingState::Complete
            );
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(
                cp.store
                    .get("goal-cleanup-parent")
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                TaskStatus::Running
            );

            // Actual paired-parent/composite transport is exercised by Matrix;
            // this bounded native-source control ensures a foreign wrapper and
            // a clean enclosing fact cannot erase this producer's negative.
            let outer = crate::agent::turn::wrap_goal_spend_refusal(
                error.context(zeroclaw_api::tool::GoalChildOperationError),
                true,
            );
            assert!(outer.is::<zeroclaw_api::tool::GoalChildOperationError>());
            let foreign = zeroclaw_api::turn_stop::tag(
                outer,
                TurnStop::fatal(TurnStopCode::BudgetExhausted, "outer native budget refusal"),
            );
            assert_eq!(
                spawn_refusal_proofs(&foreign),
                if fault {
                    vec![true, false, true]
                } else {
                    vec![true, true]
                }
            );
            assert_eq!(
                turn_stop(&foreign).unwrap().code,
                TurnStopCode::BudgetExhausted
            );
            if fault {
                assert!(format!("{foreign:#}").contains(CLEANUP_SQL_ERROR));
            }
        }
    }

    #[tokio::test]
    async fn goal_settlement_spawn_refusal_cleanup_preserves_terminal_winner_and_rejects_owner_conflict()
     {
        use crate::control_plane::TaskStatus;
        for terminal_winner in [true, false] {
            let (directory, cp, context) = goal_spawn_cleanup_fixture().await;
            if terminal_winner {
                assert!(
                    cp.store
                        .transition_terminal_if_owner(
                            CLEANUP_CHILD,
                            std::process::id(),
                            &cp.boot_id,
                            TaskStatus::Cancelled,
                            Some("winning output".into()),
                            Some("winning cancellation".into()),
                        )
                        .await
                        .unwrap()
                );
            } else {
                cp.store
                    .claim_owner(CLEANUP_CHILD, std::process::id(), "new-child-owner")
                    .await
                    .unwrap();
            }
            let before = cp.store.get_snapshot(CLEANUP_CHILD).await.unwrap().unwrap();
            let error = context
                .scope(run_goal_spawn_owned(&context, CLEANUP_CHILD, async {
                    Err(actual_checked_spawn_refusal(directory.path()).await)
                }))
                .await
                .unwrap_err();
            assert_eq!(
                spawn_refusal_proofs(&error),
                if terminal_winner {
                    vec![true]
                } else {
                    vec![false, true]
                }
            );
            if !terminal_winner {
                assert!(
                    format!("{error:#}").contains(
                        "Goal subagent terminal transition lost without a terminal winner"
                    )
                );
            }
            let after = cp.store.get_snapshot(CLEANUP_CHILD).await.unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(&after.task).unwrap(),
                serde_json::to_value(&before.task).unwrap()
            );
            assert_eq!(after.output, before.output);
            assert_eq!(after.error, before.error);
            assert_eq!(
                after.task.status,
                if terminal_winner {
                    TaskStatus::Cancelled
                } else {
                    TaskStatus::Running
                }
            );
        }
    }

    #[tokio::test]
    async fn goal_settlement_spawn_refusal_cleanup_preserves_accounting_source_metadata() {
        use zeroclaw_goal::{GoalAccountingState, accounting::GoalAccountingFailure};
        for (state, pending) in [
            (GoalAccountingState::Missing, false),
            (GoalAccountingState::Invalid, false),
            (GoalAccountingState::OutcomeUnknown, true),
            (GoalAccountingState::Complete, true),
        ] {
            let (directory, cp, context) = goal_spawn_cleanup_fixture().await;
            fail_exact_child_terminal_write(directory.path());
            let error = context
                .scope(run_goal_spawn_owned(&context, CLEANUP_CHILD, async {
                    // This typed context models an already exposed earlier-call
                    // condition; this refused child still admits and bills nothing.
                    Err(actual_checked_spawn_refusal(directory.path())
                        .await
                        .context(GoalAccountingFailure::incomplete(state, pending))
                        .context(zeroclaw_api::tool::GoalChildOperationError))
                }))
                .await
                .unwrap_err();
            assert!(error.is::<zeroclaw_api::tool::GoalChildOperationError>());
            assert_eq!(spawn_refusal_proofs(&error), vec![false, true]);
            let accounting = error
                .chain()
                .find_map(|source| source.downcast_ref::<GoalAccountingFailure>())
                .unwrap();
            assert_eq!(accounting.accounting_state(), state);
            assert_eq!(accounting.has_pending_operation(), pending);
            assert!(format!("{error:#}").contains(CLEANUP_SQL_ERROR));
            assert_eq!(
                zeroclaw_api::turn_stop::turn_stop(&error).unwrap().code,
                zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
            );
            assert_eq!(
                cp.store.get(CLEANUP_CHILD).await.unwrap().unwrap().status,
                crate::control_plane::TaskStatus::Running
            );

            // A foreign native-source boundary must retain the bridge, not
            // merely expose Anyhow's original context downcast metadata.
            let foreign = zeroclaw_api::turn_stop::tag(
                error,
                zeroclaw_api::turn_stop::TurnStop::fatal(
                    zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted,
                    "outer budget refusal",
                ),
            );
            let accounting = foreign
                .chain()
                .find_map(|source| source.downcast_ref::<GoalAccountingFailure>())
                .unwrap();
            assert_eq!(accounting.accounting_state(), state);
            assert_eq!(accounting.has_pending_operation(), pending);
            assert_eq!(spawn_refusal_proofs(&foreign), vec![false, true]);
        }
    }

    #[tokio::test]
    async fn goal_settlement_spawn_refusal_cleanup_respects_decisive_stronger_stop() {
        use zeroclaw_api::turn_stop::{TurnStop, TurnStopCode, tag, turn_stop};
        for code in [TurnStopCode::TurnTimeout, TurnStopCode::StepTimeout] {
            let (directory, cp, context) = goal_spawn_cleanup_fixture().await;
            fail_exact_child_terminal_write(directory.path());
            let error = context
                .scope(run_goal_spawn_owned(&context, CLEANUP_CHILD, async {
                    Err(tag(
                        actual_checked_spawn_refusal(directory.path()).await,
                        TurnStop::recoverable(code, "decisive outer timeout"),
                    ))
                }))
                .await
                .unwrap_err();
            assert_eq!(turn_stop(&error).unwrap().code, code);
            assert_eq!(
                spawn_refusal_proofs(&error),
                vec![true],
                "a stronger stop must not be relabelled by finding the inner budget stop"
            );
            assert!(format!("{error:#}").contains(CLEANUP_SQL_ERROR));
            assert_eq!(
                cp.store.get(CLEANUP_CHILD).await.unwrap().unwrap().status,
                crate::control_plane::TaskStatus::Running
            );
        }
    }

    #[tokio::test]
    async fn goal_settlement_spawn_cleanup_keeps_nonbudget_sibling_results() {
        use crate::control_plane::TaskStatus;
        for (success, fault) in [(true, false), (true, true), (false, true)] {
            let (directory, cp, context) = goal_spawn_cleanup_fixture().await;
            if fault {
                fail_exact_child_terminal_write(directory.path());
            }
            let result = context
                .scope(run_goal_spawn_owned(&context, CLEANUP_CHILD, async {
                    if success {
                        Ok("allowed child output".into())
                    } else {
                        anyhow::bail!("original nonbudget child error")
                    }
                }))
                .await;
            let row = cp.store.get_snapshot(CLEANUP_CHILD).await.unwrap().unwrap();
            if !fault {
                assert!(result.unwrap().success);
                assert_eq!(row.task.status, TaskStatus::Completed);
                assert_eq!(row.output.as_deref(), Some("allowed child output"));
                assert!(row.task.finished_at.is_some());
            } else {
                let error = result.unwrap_err();
                assert!(spawn_refusal_proofs(&error).is_empty());
                assert!(zeroclaw_api::turn_stop::turn_stop(&error).is_none());
                let diagnostic = format!("{error:#}");
                assert!(diagnostic.contains(CLEANUP_SQL_ERROR));
                if success {
                    assert!(!diagnostic.contains("Goal subagent cleanup failed:"));
                } else {
                    assert!(diagnostic.contains("original nonbudget child error"));
                    assert_eq!(
                        diagnostic.matches("Goal subagent cleanup failed:").count(),
                        1
                    );
                }
                assert_eq!(row.task.status, TaskStatus::Running);
                assert!(row.output.is_none());
                assert!(row.task.finished_at.is_none());
            }
        }
    }

    #[tokio::test]
    async fn goal_sentinel_without_accountant_or_root_rejects_spawn_before_effects() {
        let tool = SpawnSubagentTool::new(
            Arc::new(Config::default()),
            "unused",
            Arc::new(SecurityPolicy::default()),
        );
        let context = zeroclaw_api::tool::ToolExecutionContext::default().with_goal_foreground();
        let error = tool
            .execute_with_context(serde_json::json!({"prompt": "unused"}), &context)
            .await
            .unwrap_err();
        assert!(
            error
                .downcast_ref::<zeroclaw_api::tool::GoalChildOperationError>()
                .is_some()
        );
    }
    use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

    fn config_with_agent(alias: &str) -> Config {
        let mut config = Config::default();
        config
            .risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());
        config.agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    #[tokio::test]
    async fn goal_spawn_owned_panic_settles_exact_row_then_resumes_original_payload() {
        use crate::control_plane::{ControlPlaneHandle, TaskKind, TaskRecord, TaskStatus};
        #[derive(Debug)]
        struct PanicMarker;
        async fn panic_body() -> Result<String> {
            std::panic::panic_any(PanicMarker)
        }

        let directory = tempfile::TempDir::new().unwrap();
        let cp = ControlPlaneHandle::open(directory.path()).unwrap();
        let scope =
            zeroclaw_goal::GoalExecutionScope::new("panic-parent", "panic-session", 1).unwrap();
        let parent = TaskRecord {
            id: scope.task_id().into(),
            kind: TaskKind::Goal,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: std::process::id(),
            owner_boot_id: cp.boot_id.clone(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: Some(scope.session_id().into()),
            execution_epoch: 1,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
        };
        let goal = zeroclaw_goal::GoalTaskRecord {
            task_id: parent.id.clone(),
            objective: "panic boundary".into(),
            ..Default::default()
        };
        assert_eq!(
            cp.goal_store()
                .unwrap()
                .create_or_replace_session_goal(parent, goal)
                .await
                .unwrap(),
            zeroclaw_goal::GoalTransitionResult::Applied
        );
        let child = TaskRecord {
            id: "panic-child".into(),
            kind: TaskKind::Subagent,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: std::process::id(),
            owner_boot_id: cp.boot_id.clone(),
            heartbeat_at: None,
            depth: 1,
            parent_id: Some(scope.task_id().into()),
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: None,
            execution_epoch: 0,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
        };
        cp.store.create(child).await.unwrap();
        let root = crate::goal_accounting::GoalForegroundContext::new_root(
            scope,
            cp.clone(),
            tokio_util::sync::CancellationToken::new(),
            Some(tokio::time::Instant::now() + std::time::Duration::from_secs(60)),
        );
        let child_context = root.child();
        let panic = std::panic::AssertUnwindSafe(run_goal_spawn_owned(
            &child_context,
            "panic-child",
            panic_body(),
        ))
        .catch_unwind()
        .await
        .unwrap_err();
        assert!(
            panic.downcast::<PanicMarker>().is_ok(),
            "original panic payload must be resumed"
        );
        let row = cp.store.get_snapshot("panic-child").await.unwrap().unwrap();
        assert_eq!(row.task.status, TaskStatus::Failed);
        assert_eq!(row.task.owner_pid, std::process::id());
        assert_eq!(row.task.owner_boot_id, cp.boot_id);
        assert_eq!(row.task.parent_id.as_deref(), Some("panic-parent"));
        assert!(row.task.finished_at.is_some());
        assert!(row.output.is_none());
    }

    #[tokio::test]
    async fn goal_subagent_row_cleanup_preserves_winner_and_rejects_lost_live_owner() {
        use crate::control_plane::{ControlPlaneHandle, TaskKind, TaskRecord, TaskStatus};
        let directory = tempfile::TempDir::new().unwrap();
        let cp = ControlPlaneHandle::open(directory.path()).unwrap();
        let record = |id: &str, owner_boot_id: String| TaskRecord {
            id: id.into(),
            kind: TaskKind::Subagent,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: std::process::id(),
            owner_boot_id,
            heartbeat_at: None,
            depth: 1,
            parent_id: Some("goal-parent".into()),
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: None,
            execution_epoch: 0,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
        };
        cp.store
            .create(record("owned", cp.boot_id.clone()))
            .await
            .unwrap();
        settle_goal_subagent_row(
            &cp,
            "owned",
            TaskStatus::Completed,
            Some("result".into()),
            None,
        )
        .await
        .unwrap();
        let snapshot = cp.store.get_snapshot("owned").await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Completed);
        assert_eq!(snapshot.task.parent_id.as_deref(), Some("goal-parent"));
        assert_eq!(snapshot.output.as_deref(), Some("result"));

        cp.store
            .create(record("winner", cp.boot_id.clone()))
            .await
            .unwrap();
        assert!(
            cp.store
                .transition_terminal_if_owner(
                    "winner",
                    std::process::id(),
                    &cp.boot_id,
                    TaskStatus::Cancelled,
                    None,
                    None
                )
                .await
                .unwrap()
        );
        settle_goal_subagent_row(&cp, "winner", TaskStatus::Completed, None, None)
            .await
            .unwrap();
        assert_eq!(
            cp.store
                .get_snapshot("winner")
                .await
                .unwrap()
                .unwrap()
                .task
                .status,
            TaskStatus::Cancelled
        );

        cp.store
            .create(record("lost-owner", "different-boot".into()))
            .await
            .unwrap();
        assert!(
            settle_goal_subagent_row(&cp, "lost-owner", TaskStatus::Failed, None, None)
                .await
                .is_err()
        );
        assert_eq!(
            cp.store
                .get_snapshot("lost-owner")
                .await
                .unwrap()
                .unwrap()
                .task
                .status,
            TaskStatus::Running
        );
    }

    #[tokio::test]
    async fn closed_authority_rejects_subagent_before_construction() {
        let config = config_with_agent("alpha");
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let tool = SpawnSubagentTool::new(
            Arc::new(config),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        )
        .with_execution_capability(Some(authority.execution_capability()));
        authority.close_agent_lifecycle();

        let result = tool.execute(json!({"prompt": "hello"})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains("lifecycle generation is closing")
        );
        assert_eq!(authority.agent_lifecycle().active_turn_count("alpha"), 0);
    }

    #[tokio::test]
    async fn empty_or_missing_prompt_is_rejected() {
        let tool = SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        for args in [json!({}), json!({ "prompt": "   " })] {
            let result = tool
                .execute(args)
                .await
                .expect("execute returns Ok with structured failure");
            assert!(!result.success);
            assert!(
                result
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("prompt"),
                "expected prompt-validation error, got: {:?}",
                result.error
            );
        }
    }

    #[tokio::test]
    async fn unknown_parent_alias_surfaces_spawn_failure() {
        // Parent alias that is not configured: SubAgentSpawn::for_agent_with_policy
        // returns Err, the tool reports a structured spawn failure
        // (no panic, no recursion attempt).
        let tool = SpawnSubagentTool::new(
            Arc::new(Config::default()),
            "missing-alpha",
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("subagent spawn failed"),
            "expected spawn-failure error, got: {:?}",
            result.error
        );
    }

    // ── Depth-1 cap: subagent may not spawn its own subagent ──

    #[tokio::test]
    async fn refuses_recursive_spawn_when_caller_is_subagent() {
        let tool = SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        )
        .with_subagent_caller(true);
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            err.contains("subagent") && err.contains("depth"),
            "expected depth-cap refusal mentioning subagent + depth, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn allows_top_level_spawn_when_caller_is_not_subagent() {
        // The top-level path may still fail later for unrelated reasons
        // (e.g. no model provider configured in this minimal harness),
        // but it MUST NOT trip the depth-cap refusal. Pin that the
        // depth-cap error is absent.
        let tool = SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        )
        .with_subagent_caller(false);
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok");
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            !(err.contains("subagent") && err.contains("depth")),
            "top-level caller must not see the depth-cap refusal, got: {err:?}"
        );
    }

    // ── risk-profile tool gates spawn_subagent ──

    fn config_with_allowed_tools(alias: &str, allowed_tools: Vec<String>) -> Config {
        let mut config = Config::default();
        config.risk_profiles.insert(
            "default".to_string(),
            RiskProfileConfig {
                allowed_tools,
                ..RiskProfileConfig::default()
            },
        );
        config.agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    fn config_with_deny_all_tools(alias: &str) -> Config {
        let mut config = Config::default();
        config.risk_profiles.insert(
            "default".to_string(),
            RiskProfileConfig {
                deny_all_tools: true,
                ..RiskProfileConfig::default()
            },
        );
        config.agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    #[tokio::test]
    async fn refuses_when_risk_profile_excludes_spawn_subagent() {
        // Parent's non-empty risk_profile.allowed_tools omits
        // "spawn_subagent" — the tool itself refuses pre-spawn so the
        // dispatch-site filter doesn't have to be the only line of defense.
        let config = config_with_allowed_tools("alpha", vec!["shell".into()]);
        let tool = SpawnSubagentTool::new(
            Arc::new(config),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            err.contains("risk_profile") && err.contains("spawn_subagent"),
            "expected risk_profile-gate refusal naming spawn_subagent, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn admits_when_risk_profile_lists_spawn_subagent() {
        // When the parent's risk_profile.allowed_tools explicitly lists
        // spawn_subagent, the tool does NOT short-circuit on the gate.
        // It may still fail later for unrelated reasons; pin only that
        // the gate refusal is absent.
        let config =
            config_with_allowed_tools("alpha", vec!["spawn_subagent".into(), "shell".into()]);
        let tool = SpawnSubagentTool::new(
            Arc::new(config),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok");
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            !(err.contains("risk_profile") && err.contains("spawn_subagent")),
            "spawn_subagent in allowed_tools must not trigger the gate refusal, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn admits_when_risk_profile_allowed_tools_is_legacy_empty() {
        // `allowed_tools = []` is legacy-unrestricted, so the gate must not
        // refuse. The spawn may still fail later for unrelated reasons; pin
        // only that the gate refusal is absent.
        let config = config_with_allowed_tools("alpha", vec![]);
        let tool = SpawnSubagentTool::new(
            Arc::new(config),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok");
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            !(err.contains("risk_profile") && err.contains("spawn_subagent")),
            "legacy empty allowed_tools is unrestricted and must not trigger the gate refusal, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn refuses_when_risk_profile_deny_all_tools() {
        let config = config_with_deny_all_tools("alpha");
        let tool = SpawnSubagentTool::new(
            Arc::new(config),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            err.contains("risk_profile") && err.contains("spawn_subagent"),
            "deny_all_tools must deny spawn_subagent, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn refuses_when_risk_profile_uses_legacy_deny_all_sentinel() {
        let config = config_with_allowed_tools(
            "alpha",
            vec![
                RiskProfileConfig::LEGACY_DENY_ALL_TOOLS_SENTINEL.into(),
                RiskProfileConfig::LEGACY_DENY_ALL_TOOLS_SENTINEL.into(),
            ],
        );
        let tool = SpawnSubagentTool::new(
            Arc::new(config),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            err.contains("risk_profile") && err.contains("spawn_subagent"),
            "legacy __none__ must deny spawn_subagent, got: {err:?}"
        );
    }

    // ── Launch-side fan-out bound: shared action budget ──

    #[tokio::test]
    async fn repeated_spawns_blocked_once_action_budget_is_exhausted() {
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 2,
            ..SecurityPolicy::default()
        });
        let tool = SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::clone(&security),
        );

        for attempt in 1..=2 {
            let result = tool
                .execute(json!({ "prompt": "same fan-out prompt" }))
                .await
                .expect("execute returns Ok");
            let err = result.error.as_deref().unwrap_or_default();
            assert!(
                !err.contains("Rate limit exceeded"),
                "attempt {attempt} within budget must not be rate-limited, got: {err:?}"
            );
        }

        let result = tool
            .execute(json!({ "prompt": "same fan-out prompt" }))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Rate limit exceeded"),
            "3rd launch attempt past a budget of 2 must be refused, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn validation_failures_do_not_consume_launch_budget() {
        // The budget gate sits after prompt validation: malformed calls
        // must not burn launch slots (matching RateLimitedTool's
        // only-work-consumes-budget semantics).
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        });
        let tool = SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::clone(&security),
        );

        for _ in 0..3 {
            let result = tool
                .execute(json!({ "prompt": "   " }))
                .await
                .expect("execute returns Ok with structured failure");
            assert!(
                result
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("prompt"),
                "invalid-prompt refusal expected, got: {:?}",
                result.error
            );
        }

        let result = tool
            .execute(json!({ "prompt": "valid" }))
            .await
            .expect("execute returns Ok");
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            !err.contains("Rate limit exceeded"),
            "validation failures must not have consumed the budget, got: {err:?}"
        );
    }

    #[test]
    fn agent_run_overrides_default_is_top_level() {
        use crate::agent::loop_::AgentRunOverrides;
        let overrides = AgentRunOverrides::default();
        assert!(
            !overrides.is_subagent,
            "AgentRunOverrides::default().is_subagent must be false so cron paths inherit a top-level shape"
        );
    }

    // ── A child run may not escape its parent step's scope ──

    fn scoped_step() -> crate::sop::active_scope::HeadlessStepScope {
        use crate::sop::{SopStep, StepToolScope};
        crate::sop::active_scope::HeadlessStepScope {
            run_id: "run-1".into(),
            step: SopStep {
                number: 2,
                scope: Some(StepToolScope {
                    allow: Some(vec!["read_file".into()]),
                    deny: Vec::new(),
                }),
                ..SopStep::default()
            },
            config: zeroclaw_config::schema::SopConfig {
                step_scope_enforce: true,
                ..Default::default()
            },
        }
    }

    /// A scoped headless step may still call `spawn_subagent`, but the child it
    /// starts must not regain what the step gave up. Before this, the child ran
    /// with `sop_step_scope: None` and rebuilt the agent's whole surface,
    /// including the SOP control tools the step turn always drops — so spawning
    /// a child was a way out of the step's capability boundary.
    #[tokio::test]
    async fn child_run_inherits_the_parent_step_scope() {
        let registry = vec![
            "read_file".to_string(),
            "shell".to_string(),
            "sop_advance".to_string(),
        ];

        let inherited =
            crate::sop::active_scope::with_active_headless_step_scope(scoped_step(), async {
                child_run_overrides(Arc::new(SecurityPolicy::default())).sop_step_scope
            })
            .await
            .expect("a child started inside a headless step must carry that step's scope");

        assert_eq!(inherited.run_id, "run-1");
        assert_eq!(inherited.step.number, 2);
        let excluded = inherited.excluded(&registry);
        assert!(
            excluded.iter().any(|t| t == "shell"),
            "the step's denied tools must stay denied in the child, got {excluded:?}"
        );
        assert!(
            excluded.iter().any(|t| t == "sop_advance"),
            "the SOP control surface must stay excluded in the child, got {excluded:?}"
        );
        assert!(
            !excluded.iter().any(|t| t == "read_file"),
            "the step's allowed tool must survive into the child, got {excluded:?}"
        );
    }

    /// The inheritance is bounded by the step: an ordinary agent turn spawns a
    /// child with no step scope at all.
    #[tokio::test]
    async fn child_run_outside_a_headless_step_carries_no_scope() {
        let overrides = child_run_overrides(Arc::new(SecurityPolicy::default()));
        assert!(overrides.sop_step_scope.is_none());
        assert!(
            overrides.is_subagent,
            "a spawned child is always a subagent"
        );
    }

    #[test]
    fn spawn_subagent_dyn_tool_implements_attributable() {
        use zeroclaw_api::attribution::{Attributable, Role, ToolKind};

        let tool: Box<dyn Tool> = Box::new(SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        ));
        assert_eq!(
            Attributable::role(tool.as_ref()),
            Role::Tool(ToolKind::SpawnSubagent),
            "SpawnSubagentTool must surface its kind through the Tool trait object"
        );
        assert!(
            !Attributable::alias(tool.as_ref()).is_empty(),
            "Attributable::alias on a Tool must be non-empty so composite keys never produce `.<bare>`"
        );
    }
}
