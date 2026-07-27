//! Agent-loop tool that spawns an ephemeral SubAgent inheriting the
//! parent's identity, security policy, and memory allowlist, runs a
//! focused prompt, and returns the response. Cron's `JobType::Agent`
//! dispatch is the other SubAgent spawn site; both funnel through

use crate::agent::loop_::AgentRunOverrides;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use crate::subagent::{SubAgentOverrides, SubAgentSpawn};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
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
}

#[async_trait]
impl Tool for SpawnSubagentTool {
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

        let risk_profile = self.config.risk_profile_for_agent(&self.parent_alias);
        if let Some(rp) = risk_profile {
            let excluded = rp.excluded_tools.iter().any(|t| t == "spawn_subagent");
            let allowed_when_listed = rp.allowed_tools.is_empty()
                || rp.allowed_tools.iter().any(|t| t == "spawn_subagent");
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
            &self.config,
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

        let temperature: Option<f64> = self
            .config
            .model_provider_for_agent(&self.parent_alias)
            .and_then(|e| e.temperature);
        let session_path = std::path::PathBuf::from(format!("subagent-{run_id}"));

        let run_overrides = AgentRunOverrides {
            security: Some(subagent_ctx.policy.clone()),
            memory: None,
            is_subagent: true,
            // Sub-turn origin already skips memory injection; explicit for
            // the same future-proofing reason as `is_subagent` above.
            suppress_memory_inject: true,
            // Subagents keep a live memory backend and the memory tools; only
            // the injected context preamble is suppressed above.
            memory_free: false,
            // Subagent runs are short-lived; no cross-turn reuse contract,
            // so the per-call `connect_all` path inside `agent::run` is
            // the correct choice. The daemon heartbeat worker is the
            // only `mcp_registry` supplier.
            mcp_registry: None,
        };
        let parent_alias = subagent_ctx.parent_alias.clone();

        let cp_task_id = run_id.clone();
        if let Some(cp) = crate::control_plane::control_plane() {
            let _ = cp
                .store
                .create(crate::control_plane::TaskRecord {
                    id: cp_task_id.clone(),
                    kind: crate::control_plane::TaskKind::Subagent,
                    agent: self.parent_alias.clone(),
                    status: crate::control_plane::TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: cp.boot_id.clone(),
                    heartbeat_at: None,
                    depth: u32::from(self.is_subagent_caller),
                    parent_id: None,
                    originator_route: None,
                    delivered: false,
                    idem_key: None,
                    principal_id: None,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                })
                .await;
        }

        let run_result = Box::pin(scope!(
            agent_alias: parent_alias,
            session_key: run_id,
            =>
            crate::agent::run(
                (*self.config).clone(),
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
        ))
        .await;

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
