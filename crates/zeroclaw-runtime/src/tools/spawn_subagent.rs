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

        let child = if let Some(parent) =
            zeroclaw_api::session_events::capture::current_provider_admission()
        {
            parent
                .begin_child(zeroclaw_api::session_events::capture::ChildDispatchIntent {
                    relation: "spawn_subagent".into(),
                    target_agent: self.parent_alias.clone(),
                    prompt: prompt.clone(),
                    execution_mode: zeroclaw_api::session_events::ChildExecutionMode::Sync,
                    task_id: Some(cp_task_id.clone()),
                })
                .await
                .map(Some)
        } else {
            Ok(None)
        };
        let engine = Box::pin(scope!(
            agent_alias: parent_alias,
            session_key: run_id,
            =>
            crate::agent::run(
                (*config).clone(),
                &self.parent_alias,
                Some(prompt.clone()),
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

        let run_result = match child {
            Ok(Some(child)) => child.execute(engine).await,
            Ok(None) => engine.await,
            Err(error) => Err(error),
        };

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
            Err(error)
                if zeroclaw_api::session_events::capture::is_capture_stopped(&error)
                    || error.chain().any(|source| {
                        source.is::<zeroclaw_kernel::session::SessionError>()
                            || source.is::<zeroclaw_api::session_events::AppendError>()
                            || source.is::<zeroclaw_api::session_events::ReadError>()
                    }) =>
            {
                Err(error)
            }
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
    async fn native_subagent_capture_failure_stops_parent_without_fabricated_tool_failure() {
        use crate::agent::dispatcher::NativeToolDispatcher;
        use crate::live_config_authority::{AgentHistoryAuthority, LiveConfigAuthority};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use zeroclaw_api::ingress::{IngressContext, TurnOrigin};
        use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, ModelProvider, ToolCall};
        use zeroclaw_api::session_events::capture::*;
        use zeroclaw_api::session_events::*;
        use zeroclaw_kernel::session::engine::EngineSessionSource;

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Failure {
            Capture,
            Session,
            Append,
            Read,
            Business,
        }
        impl Failure {
            fn error(self) -> anyhow::Error {
                let detail = "injected_subagent_history_failure".to_string();
                let error: anyhow::Error = match self {
                    Self::Capture => capture_stopped(anyhow::Error::msg(detail)),
                    Self::Session => {
                        zeroclaw_kernel::session::SessionError::StoreUnhealthy(detail).into()
                    }
                    Self::Append => AppendError::StorageUnavailable(detail).into(),
                    Self::Read => ReadError::StorageUnavailable(detail).into(),
                    Self::Business => anyhow::Error::msg("ordinary_subagent_failure"),
                };
                error.context("subagent failure boundary")
            }
        }

        struct FailingChild(Failure);
        #[async_trait]
        impl ChildExecutionPort for FailingChild {
            async fn execute(&self, _engine: EngineTurnFuture<'_>) -> Result<String> {
                Err(self.0.error())
            }
        }
        struct FaultBoundary {
            inner: Arc<dyn ProviderAdmissionPort>,
            failure: Failure,
            at_begin: bool,
            calls: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl ProviderAdmissionPort for FaultBoundary {
            async fn admit(
                &self,
                intent: ProviderDispatchIntent,
            ) -> Result<Arc<dyn ProviderObservationPort>> {
                self.inner.admit(intent).await
            }
            async fn begin_child(
                &self,
                _intent: ChildDispatchIntent,
            ) -> Result<Arc<dyn ChildExecutionPort>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.at_begin {
                    Err(self.failure.error())
                } else {
                    Ok(Arc::new(FailingChild(self.failure)))
                }
            }
            async fn begin_context_transform(
                &self,
                intent: ContextTransformIntent,
            ) -> Result<Option<Arc<dyn ContextTransformPort>>> {
                self.inner.begin_context_transform(intent).await
            }
        }

        struct ParentProvider(Arc<AtomicUsize>);
        #[async_trait]
        impl ModelProvider for ParentProvider {
            async fn chat_with_system(
                &self,
                _system: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> Result<String> {
                anyhow::bail!("fixture requires structured parent requests")
            }
            fn supports_native_tools(&self) -> bool {
                true
            }
            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> Result<ChatResponse> {
                let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
                Ok(ChatResponse {
                    output_blocks: Vec::new(),
                    text: (!first).then(|| "parent continues".into()),
                    tool_calls: if first {
                        vec![ToolCall {
                            id: "subagent-call".into(),
                            name: SpawnSubagentTool::NAME.into(),
                            arguments: json!({"prompt":"focused child task"}).to_string(),
                            extra_content: None,
                        }]
                    } else {
                        vec![]
                    },
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
        impl zeroclaw_api::attribution::Attributable for ParentProvider {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Provider(
                    zeroclaw_api::attribution::ProviderKind::Model(
                        zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "custom.parent"
            }
        }

        for backend in ["sqlite", "jsonl"] {
            for at_begin in [true, false] {
                for failure in [
                    Failure::Capture,
                    Failure::Session,
                    Failure::Append,
                    Failure::Read,
                    Failure::Business,
                ] {
                    let directory = tempfile::tempdir().unwrap();
                    let mut config = config_with_agent("alpha");
                    config.config_path = directory.path().join("config.toml");
                    config.data_dir = directory.path().to_path_buf();
                    config.channels.session_backend = backend.into();
                    config.agents.get_mut("alpha").unwrap().model_provider = "custom.parent".into();
                    config
                        .providers
                        .models
                        .ensure("custom", "parent")
                        .unwrap()
                        .model = Some("parent-model".into());
                    let security = Arc::new(SecurityPolicy::default());
                    let provider_calls = Arc::new(AtomicUsize::new(0));
                    let child_calls = Arc::new(AtomicUsize::new(0));
                    let mut agent = crate::agent::Agent::builder()
                        .agent_alias("alpha".into())
                        .model_provider_name("custom.parent".into())
                        .model_name("parent-model".into())
                        .model_provider(Box::new(ParentProvider(Arc::clone(&provider_calls))))
                        .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                            vec![Box::new(SpawnSubagentTool::new(
                                Arc::new(config.clone()),
                                "alpha",
                                security,
                            ))],
                        ))
                        .tool_dispatcher(Box::new(NativeToolDispatcher))
                        .memory(Arc::new(zeroclaw_memory::NoneMemory::new("subagent-test")))
                        .observer(Arc::new(crate::observability::NoopObserver))
                        .workspace_dir(directory.path().to_path_buf())
                        .config(config.resolved_agent_config("alpha").unwrap())
                        .build()
                        .unwrap();
                    let authority = LiveConfigAuthority::new(config);
                    let source =
                        zeroclaw_session_host::NativeSessionHub::new(authority.live_handle());
                    let family = "subagent-capture-failure";
                    let stream = source.stream_id(family).await.unwrap();
                    let principal = zeroclaw_api::principal::PrincipalId::shared_operator();
                    let port = source
                        .open(
                            family.into(),
                            "alpha".into(),
                            principal.clone(),
                            Arc::new(AgentHistoryAuthority {
                                admission: authority
                                    .execution_capability()
                                    .admit("alpha")
                                    .unwrap()
                                    .into(),
                                actor: EventActor::Principal {
                                    principal_id: principal.0,
                                },
                                stream: stream.clone(),
                                origin: TurnOrigin::Interactive,
                                parent_alias: None,
                            }),
                            ContextSnapshot {
                                messages: vec![],
                                history_has_trim_breadcrumb: Some(false),
                            },
                        )
                        .await
                        .unwrap();
                    let result = port
                        .execute(
                            EngineTurnInput {
                                execution_workspace: None,
                                turn_id: "parent-turn".into(),
                                agent_alias: Some("alpha".into()),
                                ingress: IngressContext::from_origin(TurnOrigin::Interactive),
                                reply_route: None,
                                parts: vec![EngineInputPart::Text("delegate this task".into())],
                                provider_ref: "custom.parent".into(),
                                model: "parent-model".into(),
                            },
                            Box::pin(async {
                                let inner = current_provider_admission().unwrap();
                                scope_provider_capture(
                                    Arc::new(FaultBoundary {
                                        inner,
                                        failure,
                                        at_begin,
                                        calls: Arc::clone(&child_calls),
                                    }),
                                    agent.turn("delegate this task"),
                                )
                                .await
                            }),
                        )
                        .await;
                    let ordinary = failure == Failure::Business;
                    assert_eq!(
                        result.is_ok(),
                        ordinary,
                        "{backend}/{at_begin}/{failure:?}: {result:?}"
                    );
                    assert_eq!(child_calls.load(Ordering::SeqCst), 1);
                    assert_eq!(
                        provider_calls.load(Ordering::SeqCst),
                        if ordinary { 2 } else { 1 }
                    );

                    let store = zeroclaw_infra::session_events::family::FamilyEventStore::open(
                        directory.path(),
                        family,
                    )
                    .unwrap();
                    let events = store
                        .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 256)
                        .unwrap()
                        .events;
                    let started = events
                        .iter()
                        .find(|event| {
                            matches!(event.body.data, EventData::ToolEffectStarted { .. })
                        })
                        .unwrap();
                    let terminal = events
                        .iter()
                        .find(|event| {
                            event.invocation_id == started.invocation_id
                                && matches!(
                                    event.body.data,
                                    EventData::ToolEffectFailed { .. }
                                        | EventData::InvocationOutcomeUnknown { .. }
                                )
                        })
                        .unwrap();
                    if ordinary {
                        assert!(matches!(
                            terminal.body.data,
                            EventData::ToolEffectFailed { .. }
                        ));
                    } else {
                        let EventData::InvocationOutcomeUnknown {
                            error_ref: Some(error_ref),
                            ..
                        } = &terminal.body.data
                        else {
                            panic!(
                                "capture failure must retain an uncertain tool outcome: {terminal:?}"
                            );
                        };
                        let PayloadRead::Present(diagnostic) =
                            store.read_payload(&error_ref.payload_id).unwrap()
                        else {
                            panic!("capture diagnostic missing");
                        };
                        assert!(
                            String::from_utf8_lossy(&diagnostic.body)
                                .contains("injected_subagent_history_failure")
                        );
                        assert!(events.iter().any(|event| matches!(
                            event.body.data,
                            EventData::TurnOutcomeUnknown { .. }
                        )));
                    }
                }
            }
        }
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
