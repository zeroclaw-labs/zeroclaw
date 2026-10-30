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

/// Whether a risk profile admits `tool`: not excluded, and listed when the
/// profile restricts tools to an allow-list. The same rule gates
/// `spawn_subagent` itself and the model choice a routed spawn makes.
fn risk_profile_admits(rp: &zeroclaw_config::schema::RiskProfileConfig, tool: &str) -> bool {
    !rp.excluded_tools.iter().any(|t| t == tool)
        && rp
            .effective_allowed_tools()
            .is_none_or(|tools| tools.iter().any(|t| t == tool))
}

/// Whether the parent may choose its child's model, judged exactly as its own
/// `model_switch` call would be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteAuthorization {
    Allowed,
    /// The profile does not admit `model_switch`, or the agent cannot act.
    NotPermitted,
    /// Admitted, but switching models requires an approval this tool cannot
    /// obtain on the operator's behalf.
    NeedsApproval,
}

/// A route sends the child's requests, including anything the parent puts in
/// its prompt, to another provider. Being allowed to spawn is not permission
/// to choose that provider: a private agent that may not switch models must
/// not reach a route declared for another agent through its subagents.
///
/// Approval is decided by the same `ApprovalManager` rule `model_switch`
/// itself is subject to, so `always_ask` wins even under Full autonomy. When
/// switching would need a prompt, the routed spawn is refused rather than
/// allowed through on `spawn_subagent`'s own approval: an auto-approved spawn
/// must not stand in for a switch the operator asked to approve. A fresh
/// manager carries no session "always" grants, so this errs towards refusal.
///
/// The profile is not the whole boundary: an SOP step (headless or live) may
/// enforce a narrower tool scope. A step that denies `model_switch` cannot
/// choose a model directly, so it cannot choose one for its child either.
fn route_authorization(config: &Config, parent_alias: &str) -> RouteAuthorization {
    use crate::approval::{ApprovalManager, ApprovalRequirement};
    use crate::tools::model_switch::ModelSwitchTool;

    let Some(rp) = config.risk_profile_for_agent(parent_alias) else {
        return RouteAuthorization::NotPermitted;
    };
    let step_denies_switch =
        crate::sop::active_scope::active_headless_step_scope().is_some_and(|scope| {
            scope
                .excluded(&[ModelSwitchTool::NAME.to_string()])
                .iter()
                .any(|tool| tool == ModelSwitchTool::NAME)
        });
    if step_denies_switch
        || rp.level == zeroclaw_config::autonomy::AutonomyLevel::ReadOnly
        || !risk_profile_admits(rp, ModelSwitchTool::NAME)
    {
        return RouteAuthorization::NotPermitted;
    }
    match ApprovalManager::from_risk_profile(rp).approval_requirement(ModelSwitchTool::NAME) {
        ApprovalRequirement::Prompt => RouteAuthorization::NeedsApproval,
        _ => RouteAuthorization::Allowed,
    }
}

/// Resolve an optional `model_hint` argument to the `(model_provider, model)`
/// pair of an operator-declared `[[model_routes]]` entry.
///
/// The hint is the only way a subagent leaves its parent's model, and it can
/// only name a route the operator declared. A free-form provider argument
/// would let the model send the workspace it can read to any provider the
/// config happens to hold, including one meant for a different agent with a
/// different trust posture. A route is the operator saying "this purpose may
/// use this provider".
///
/// `Ok(None)` means no hint: the subagent runs on the parent's model, exactly
/// as before. Anything else that is not a declared hint is an error rather
/// than a silent fallback, so neither a typo nor a malformed value can quietly
/// put the subtask back on the expensive model the caller was avoiding.
///
/// "No hint" is the key being absent or `null`; `null` is how many
/// tool-calling models express an optional parameter they chose not to set.
/// Arguments reach `execute` without schema validation, so every other shape
/// (a blank string, a number, an object) is a present-but-invalid hint and is
/// refused, never read as omitted.
fn resolve_model_hint(
    config: &Config,
    args: &serde_json::Value,
) -> std::result::Result<Option<(String, String)>, String> {
    let hint = match args.get("model_hint") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(value) => match value.as_str().map(str::trim) {
            Some(hint) if !hint.is_empty() => hint,
            _ => {
                return Err(crate::i18n::get_required_cli_string(
                    "tool-spawn-subagent-error-model-hint-invalid",
                ));
            }
        },
    };
    match config.model_routes.iter().find(|route| route.hint == hint) {
        Some(route) => Ok(Some((route.model_provider.clone(), route.model.clone()))),
        None => {
            let known: Vec<&str> = config
                .model_routes
                .iter()
                .map(|route| route.hint.as_str())
                .collect();
            Err(if known.is_empty() {
                crate::i18n::get_required_cli_string_with_args(
                    "tool-spawn-subagent-error-model-hint-no-routes",
                    &[("hint", hint)],
                )
            } else {
                crate::i18n::get_required_cli_string_with_args(
                    "tool-spawn-subagent-error-model-hint-unknown",
                    &[("hint", hint), ("hints", known.join(", ").as_str())],
                )
            })
        }
    }
}

#[async_trait]
impl Tool for SpawnSubagentTool {
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
        let mut schema = json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task or question for the SubAgent. Be specific and self-contained — the SubAgent does not see this conversation's history."
                }
            },
            "required": ["prompt"]
        });
        // Offered only when the operator declared routes, and enumerated so
        // the model can only pick one of them: an unused optional parameter is
        // noise in every request, and a free-text one invites invented hints.
        let hints: Vec<&str> = self
            .config
            .model_routes
            .iter()
            .map(|route| route.hint.as_str())
            .collect();
        // Also offered only to a parent that may choose a model at all; the
        // execute path re-checks against the admitted config regardless.
        if !hints.is_empty()
            && route_authorization(&self.config, &self.parent_alias) == RouteAuthorization::Allowed
        {
            schema["properties"]["model_hint"] = json!({
                "type": "string",
                "enum": hints,
                "description": "Optional. Run the SubAgent on the model of an operator-declared \
                    route instead of this agent's own model, e.g. a lighter or lower-reasoning \
                    one for broad, mechanical passes. Omit to use this agent's model."
            });
        }
        schema
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
        if let Some(rp) = risk_profile
            && !risk_profile_admits(rp, Self::NAME)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "spawn_subagent: refused — agent '{}' risk_profile does not list spawn_subagent in allowed_tools",
                    self.parent_alias
                )),
            });
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

        let route = match resolve_model_hint(&config, &args) {
            Ok(route) => route,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error),
                });
            }
        };
        if route.is_some() {
            let refusal = match route_authorization(&config, &self.parent_alias) {
                RouteAuthorization::Allowed => None,
                RouteAuthorization::NotPermitted => {
                    Some("tool-spawn-subagent-error-model-hint-not-permitted")
                }
                RouteAuthorization::NeedsApproval => {
                    Some("tool-spawn-subagent-error-model-hint-needs-approval")
                }
            };
            if let Some(key) = refusal {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(crate::i18n::get_required_cli_string(key)),
                });
            }
        }
        let (provider_override, model_override) = route.unzip();

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

        // Sampling belongs to the provider that serves the run: a routed
        // subagent takes its route's temperature, not its parent's.
        let temperature: Option<f64> = match provider_override.as_deref() {
            Some(provider_ref) => provider_ref
                .split_once('.')
                .and_then(|(ty, alias)| config.providers.models.find(ty, alias))
                .and_then(|e| e.temperature),
            None => config
                .model_provider_for_agent(&self.parent_alias)
                .and_then(|e| e.temperature),
        };
        let session_path = std::path::PathBuf::from(format!("subagent-{run_id}"));

        let mut run_overrides = child_run_overrides(subagent_ctx.policy.clone());
        run_overrides.execution_admission = execution_admission.clone();
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
                    originator_chain: Vec::new(),
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
                (*config).clone(),
                &self.parent_alias,
                Some(prompt),
                provider_override,
                model_override,
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

    /// An agent with one declared route that is also allowed to choose a
    /// model: `model_switch` auto-approved, which the default Supervised
    /// profile does not do on its own.
    fn config_with_route(alias: &str, hint: &str) -> Config {
        let mut config = config_with_agent(alias);
        config
            .model_routes
            .push(zeroclaw_config::schema::ModelRouteConfig {
                hint: hint.into(),
                model_provider: "custom.light".into(),
                model: "light-model".into(),
                ..Default::default()
            });
        authorize_model_switch(&mut config);
        config
    }

    fn authorize_model_switch(config: &mut Config) {
        config
            .risk_profiles
            .get_mut("default")
            .expect("fixture profile")
            .auto_approve
            .push("model_switch".into());
    }

    fn profile(config: &mut Config) -> &mut RiskProfileConfig {
        config
            .risk_profiles
            .get_mut("default")
            .expect("fixture profile")
    }

    /// The parent's own `model_switch` authority decides whether it may pick a
    /// route, through the same admission and approval rules that call obeys.
    #[test]
    fn route_authorization_follows_the_parents_model_switch_authority() {
        use zeroclaw_config::autonomy::AutonomyLevel;

        let mut config = config_with_route("alpha", "light");
        assert_eq!(
            route_authorization(&config, "alpha"),
            RouteAuthorization::Allowed
        );

        // Supervised default: switching is admitted but needs a prompt.
        let mut supervised = config_with_agent("alpha");
        assert_eq!(
            route_authorization(&supervised, "alpha"),
            RouteAuthorization::NeedsApproval
        );

        // always_ask wins even under Full autonomy and over auto_approve.
        profile(&mut config).level = AutonomyLevel::Full;
        profile(&mut config).always_ask.push("model_switch".into());
        assert_eq!(
            route_authorization(&config, "alpha"),
            RouteAuthorization::NeedsApproval
        );

        // Excluded, or absent from an allow-list: not permitted at all.
        profile(&mut supervised)
            .excluded_tools
            .push("model_switch".into());
        assert_eq!(
            route_authorization(&supervised, "alpha"),
            RouteAuthorization::NotPermitted
        );
        let mut listed = config_with_route("alpha", "light");
        profile(&mut listed).allowed_tools = vec!["file_read".into(), "spawn_subagent".into()];
        assert_eq!(
            route_authorization(&listed, "alpha"),
            RouteAuthorization::NotPermitted
        );

        // A read-only agent cannot act, so it cannot choose a provider either.
        let mut read_only = config_with_route("alpha", "light");
        profile(&mut read_only).level = AutonomyLevel::ReadOnly;
        assert_eq!(
            route_authorization(&read_only, "alpha"),
            RouteAuthorization::NotPermitted
        );
    }

    /// A parent that may not choose a model is never offered the parameter,
    /// even when routes exist.
    #[test]
    fn the_schema_hides_model_hint_from_a_parent_that_may_not_switch() {
        let mut config = config_with_agent("alpha");
        config
            .model_routes
            .push(zeroclaw_config::schema::ModelRouteConfig {
                hint: "light".into(),
                model_provider: "custom.light".into(),
                model: "light-model".into(),
                ..Default::default()
            });
        let tool = SpawnSubagentTool::new(
            Arc::new(config),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        assert!(
            tool.parameters_schema()["properties"]
                .get("model_hint")
                .is_none(),
            "a parent needing approval to switch must not be offered routes"
        );
    }

    /// Only an absent key or `null` means "no hint". `null` is how many
    /// tool-calling models express an optional parameter left unset.
    #[test]
    fn no_model_hint_keeps_the_parent_model() {
        let config = config_with_route("alpha", "light");
        for args in [
            json!({"prompt": "p"}),
            json!({"prompt": "p", "model_hint": null}),
        ] {
            assert_eq!(resolve_model_hint(&config, &args), Ok(None), "{args}");
        }
    }

    /// A hint that is present but malformed must be refused, never read as
    /// omitted: arguments reach `execute` without schema validation, and
    /// treating these as "no hint" would silently run on the parent's model,
    /// the cost the caller was trying to avoid.
    #[test]
    fn a_present_but_malformed_hint_is_refused_not_ignored() {
        let config = config_with_route("alpha", "light");
        let invalid =
            crate::i18n::get_required_cli_string("tool-spawn-subagent-error-model-hint-invalid");
        for hint in [
            json!(""),
            json!("   "),
            json!(42),
            json!(true),
            json!(["light"]),
            json!({"hint": "light"}),
        ] {
            assert_eq!(
                resolve_model_hint(&config, &json!({"prompt": "p", "model_hint": hint})),
                Err(invalid.clone()),
                "{hint} must be refused as malformed"
            );
        }
    }

    #[test]
    fn a_declared_hint_resolves_to_its_route() {
        let config = config_with_route("alpha", "light");
        assert_eq!(
            resolve_model_hint(&config, &json!({"prompt": "p", "model_hint": "light"})),
            Ok(Some(("custom.light".into(), "light-model".into())))
        );
    }

    /// An undeclared hint must fail loudly, not fall back to the parent's
    /// model: the operator's route list is the whole permission.
    #[test]
    fn an_undeclared_hint_is_refused_and_names_the_valid_ones() {
        let config = config_with_route("alpha", "light");
        assert_eq!(
            resolve_model_hint(&config, &json!({"model_hint": "cloud"})),
            Err(crate::i18n::get_required_cli_string_with_args(
                "tool-spawn-subagent-error-model-hint-unknown",
                &[("hint", "cloud"), ("hints", "light")],
            )),
            "an undeclared hint must be refused, naming the valid ones"
        );

        assert_eq!(
            resolve_model_hint(&config_with_agent("alpha"), &json!({"model_hint": "x"})),
            Err(crate::i18n::get_required_cli_string_with_args(
                "tool-spawn-subagent-error-model-hint-no-routes",
                &[("hint", "x")],
            )),
            "with no routes, every hint is undeclared"
        );
    }

    #[test]
    fn the_schema_offers_model_hint_only_when_routes_exist() {
        let without = SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        assert!(
            without.parameters_schema()["properties"]
                .get("model_hint")
                .is_none(),
            "no routes, no parameter"
        );

        let with = SpawnSubagentTool::new(
            Arc::new(config_with_route("alpha", "light")),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        assert_eq!(
            with.parameters_schema()["properties"]["model_hint"]["enum"],
            json!(["light"]),
            "the model may only choose among declared hints"
        );
    }

    /// Refusal happens before any spawn work, so a bad hint costs nothing and
    /// cannot start a run on the parent's model by accident.
    #[tokio::test]
    async fn an_undeclared_hint_is_refused_before_spawning() {
        let tool = SpawnSubagentTool::new(
            Arc::new(config_with_agent("alpha")),
            "alpha",
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({"prompt": "hello", "model_hint": "light"}))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        assert_eq!(
            result.error,
            Some(crate::i18n::get_required_cli_string_with_args(
                "tool-spawn-subagent-error-model-hint-no-routes",
                &[("hint", "light")],
            ))
        );

        // The reported case, through the real entry point: a malformed hint
        // must be refused before spawning, not run on the parent's model.
        let result = tool
            .execute(json!({"prompt": "hello", "model_hint": "   "}))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        assert_eq!(
            result.error,
            Some(crate::i18n::get_required_cli_string(
                "tool-spawn-subagent-error-model-hint-invalid"
            ))
        );
    }

    /// What one stub provider received: request headers and JSON bodies.
    type Received = Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, serde_json::Value)>>>;

    /// An OpenAI-compatible stub that records every chat request and answers
    /// "done", so a real child run completes against it.
    async fn recording_provider() -> (String, Received, tokio::task::JoinHandle<()>) {
        use axum::{Router, extract::State, routing::post};

        async fn record(
            State(received): State<Received>,
            headers: axum::http::HeaderMap,
            axum::Json(body): axum::Json<serde_json::Value>,
        ) -> axum::Json<serde_json::Value> {
            received.lock().unwrap().push((headers, body));
            axum::Json(json!({"choices": [{"message": {"content": "done"}}]}))
        }

        let received: Received = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stub provider should bind");
        let addr = listener.local_addr().expect("stub provider address");
        let app = Router::new()
            .route("/v1/chat/completions", post(record))
            .with_state(Arc::clone(&received));
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("stub provider runs");
        });
        (format!("http://{addr}"), received, server)
    }

    /// A real parent with its own provider and one route to another, run
    /// under temp dirs so the child's `agent::run` writes nowhere real.
    fn routed_dispatch_config(tmp: &std::path::Path, parent_uri: &str, route_uri: &str) -> Config {
        use zeroclaw_config::schema::{ModelProviderConfig, OllamaModelProviderConfig};

        let mut config = Config {
            data_dir: tmp.join("data"),
            config_path: tmp.join("config.toml"),
            ..config_with_agent("alpha")
        };
        config.providers.models.ollama.insert(
            "parent".into(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("parent-model".into()),
                    uri: Some(parent_uri.into()),
                    timeout_secs: Some(5),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        config.providers.models.ollama.insert(
            "light".into(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("light-model".into()),
                    uri: Some(route_uri.into()),
                    timeout_secs: Some(5),
                    temperature: Some(0.25),
                    extra_headers: [("x-route-option".to_string(), "light".to_string())]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        config
            .agents
            .get_mut("alpha")
            .expect("fixture agent")
            .model_provider = "ollama.parent".into();
        config
            .model_routes
            .push(zeroclaw_config::schema::ModelRouteConfig {
                hint: "light".into(),
                model_provider: "ollama.light".into(),
                model: "light-model".into(),
                ..Default::default()
            });
        config
    }

    async fn spawn_routed(config: Config) -> ToolResult {
        let security =
            SecurityPolicy::for_agent(&config, "alpha").expect("fixture policy resolves");
        SpawnSubagentTool::new(Arc::new(config), "alpha", Arc::new(security))
            .execute(json!({"prompt": "list the changed files", "model_hint": "light"}))
            .await
            .expect("execute returns a structured result")
    }

    /// Through production dispatch: an authorized route sends the child's
    /// requests to the route's provider, carrying that provider's own
    /// options, and nothing to the parent's.
    #[tokio::test]
    async fn an_authorized_route_reaches_the_routes_provider_with_its_options() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (parent_uri, parent_received, parent_server) = recording_provider().await;
        let (route_uri, route_received, route_server) = recording_provider().await;
        let mut config = routed_dispatch_config(tmp.path(), &parent_uri, &route_uri);
        authorize_model_switch(&mut config);

        let result = spawn_routed(config).await;
        parent_server.abort();
        route_server.abort();

        assert!(result.success, "routed child should complete: {result:?}");
        let route = route_received.lock().unwrap();
        assert!(
            !route.is_empty(),
            "the route's provider must receive the child"
        );
        for (headers, body) in route.iter() {
            assert_eq!(body["model"], "light-model");
            assert_eq!(body["temperature"], 0.25, "the route's temperature applies");
            assert_eq!(
                headers.get("x-route-option").map(|v| v.to_str().unwrap()),
                Some("light"),
                "a provider-owned option of the route must reach the wire"
            );
        }
        assert!(
            parent_received.lock().unwrap().is_empty(),
            "a routed child must not call the parent's provider"
        );
    }

    /// The reported gap: a parent that may not switch models must not reach
    /// another provider through a subagent. Nothing is sent anywhere.
    #[tokio::test]
    async fn a_denied_route_sends_no_request_to_the_alternate_provider() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (parent_uri, parent_received, parent_server) = recording_provider().await;
        let (route_uri, route_received, route_server) = recording_provider().await;

        // Excluded outright, and separately: admitted but `always_ask`, which
        // an auto-approved spawn must not stand in for.
        let mut excluded = routed_dispatch_config(tmp.path(), &parent_uri, &route_uri);
        profile(&mut excluded)
            .excluded_tools
            .push("model_switch".into());
        let mut always_ask = routed_dispatch_config(tmp.path(), &parent_uri, &route_uri);
        authorize_model_switch(&mut always_ask);
        profile(&mut always_ask)
            .auto_approve
            .push("spawn_subagent".into());
        profile(&mut always_ask)
            .always_ask
            .push("model_switch".into());

        for (config, key) in [
            (
                excluded,
                "tool-spawn-subagent-error-model-hint-not-permitted",
            ),
            (
                always_ask,
                "tool-spawn-subagent-error-model-hint-needs-approval",
            ),
        ] {
            let result = spawn_routed(config).await;
            assert!(!result.success, "{key}: the routed spawn must be refused");
            assert_eq!(
                result.error,
                Some(crate::i18n::get_required_cli_string(key))
            );
        }
        parent_server.abort();
        route_server.abort();

        assert!(
            route_received.lock().unwrap().is_empty(),
            "a denied route must send nothing to the alternate provider"
        );
        assert!(
            parent_received.lock().unwrap().is_empty(),
            "the refusal precedes spawning, so no child runs at all"
        );
    }

    /// The profile allows and auto-approves switching, but the enforced step
    /// the parent is running denies `model_switch` while permitting spawning.
    /// The step cannot switch directly, so it cannot route a child either:
    /// the route's provider would receive the step's data in the prompt.
    /// Unhinted spawning stays available on the parent's own model.
    #[tokio::test]
    async fn a_step_that_denies_model_switch_cannot_route_a_child() {
        use crate::sop::{SopStep, StepToolScope};

        let tmp = tempfile::tempdir().expect("temp dir");
        let (parent_uri, parent_received, parent_server) = recording_provider().await;
        let (route_uri, route_received, route_server) = recording_provider().await;
        let mut config = routed_dispatch_config(tmp.path(), &parent_uri, &route_uri);
        authorize_model_switch(&mut config);
        let step = || crate::sop::active_scope::HeadlessStepScope {
            run_id: "run-1".into(),
            step: SopStep {
                number: 1,
                scope: Some(StepToolScope {
                    allow: None,
                    deny: vec!["model_switch".into()],
                }),
                ..SopStep::default()
            },
            config: zeroclaw_config::schema::SopConfig {
                step_scope_enforce: true,
                ..Default::default()
            },
        };
        let security =
            Arc::new(SecurityPolicy::for_agent(&config, "alpha").expect("fixture policy resolves"));
        let tool = SpawnSubagentTool::new(Arc::new(config), "alpha", security);

        let (routed, unhinted) =
            crate::sop::active_scope::with_active_headless_step_scope(step(), async {
                (
                    tool.execute(json!({"prompt": "summarize", "model_hint": "light"}))
                        .await
                        .expect("structured result"),
                    tool.execute(json!({"prompt": "summarize"}))
                        .await
                        .expect("structured result"),
                )
            })
            .await;
        let schema = crate::sop::active_scope::with_active_headless_step_scope(step(), async {
            tool.parameters_schema()
        })
        .await;
        parent_server.abort();
        route_server.abort();

        assert_eq!(
            routed.error,
            Some(crate::i18n::get_required_cli_string(
                "tool-spawn-subagent-error-model-hint-not-permitted"
            )),
            "the step's denial of model_switch must refuse the route"
        );
        assert!(
            route_received.lock().unwrap().is_empty(),
            "nothing may reach the alternate provider from a step that may not switch"
        );
        assert!(
            schema["properties"].get("model_hint").is_none(),
            "the step must not be offered routes"
        );
        assert!(
            unhinted.success,
            "unhinted spawning stays available: {unhinted:?}"
        );
        assert!(
            !parent_received.lock().unwrap().is_empty(),
            "the unhinted child runs on the parent's provider"
        );
    }
}
