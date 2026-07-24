//! The resolved per-agent execution context the turn engine requires.

use std::sync::{Arc, Mutex};

use zeroclaw_api::model_provider::{ChatRequest, ChatResponse};
use zeroclaw_config::schema::{MultimodalConfig, PacingConfig};
use zeroclaw_providers::{ModelProvider, ProviderDispatch, multimodal};

use super::{LoopKnobs, ModelSwitchCallback};
use crate::agent::tool_receipts::ReceiptGenerator;
use crate::approval::ApprovalManager;
use crate::hooks::HookRunner;
use crate::observability::Observer;
use crate::tools::{ActivatedToolSet, Tool};

/// The resolved model binding: which provider, model, and temperature a turn
/// uses. The base layer any LLM call needs; [`ResolvedAgentExecution`] composes
/// it. Field names mirror the engine's former flat fields so the loop body is
/// unchanged after destructuring.
pub struct ResolvedModelAccess<'a> {
    pub model_provider: &'a dyn ModelProvider,
    pub provider_name: &'a str,
    pub model: &'a str,
    pub temperature: Option<f64>,
}

impl ResolvedModelAccess<'_> {
    pub async fn run_model_query(&self, request: ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        // Fail closed before spending a provider call when the enclosing turn's
        // cost budget is already exhausted. No-op when unscoped.
        crate::agent::turn::provider_call::enforce_tool_loop_budget().await?;
        // This one-shot seam does NOT run `prepare_messages_for_provider` (the
        // main iteration path does that upstream), so a tool-result
        // `[AUDIO:/path]` in the history — e.g. the max-iteration graceful
        // summary sends the accumulated history verbatim — would otherwise
        // reach the provider as a raw filesystem path and be hallucinated
        // over. Strip loadable audio markers here so every direct
        // `run_model_query` caller is covered. Borrows untouched when clean.
        let ChatRequest {
            messages,
            tools,
            thinking,
        } = request;
        let sanitized = multimodal::sanitize_audio_markers(messages);
        let request = ChatRequest {
            messages: &sanitized,
            tools,
            thinking,
        };
        let resp = ProviderDispatch::from_ref(self.model_provider)
            .chat(request, self.model, self.temperature)
            .await?;
        // Record spend immediately after the call (before any caller-side output
        // validation) so a downstream failure still counts the provider usage.
        if let Err(error) = crate::agent::cost::record_tool_loop_cost_usage_optional(
            self.provider_name,
            self.model,
            resp.usage.as_ref(),
        )
        .await
        {
            if let Some(task_id) = crate::agent::cost::current_exact_goal_task_id() {
                let _ =
                    crate::control_plane::pause_goal_for_accounting_failure(&task_id, &error).await;
            }
            return Err(error);
        }
        Ok(resp)
    }
}

pub struct ResolvedAgentExecution<'a> {
    /// Provider + model + temperature.
    pub model_access: ResolvedModelAccess<'a>,
    /// The tools available this turn (gated per the agent's policy upstream).
    pub tools_registry: &'a [Box<dyn Tool>],
    /// Telemetry/audit sink.
    pub observer: &'a dyn Observer,
    /// Suppress stderr output (subagents/reviews run silent).
    pub silent: bool,
    /// Approval policy + back-channel; `None` for paths that never prompt.
    pub approval: Option<&'a ApprovalManager>,
    /// Vision-model routing config.
    pub multimodal_config: &'a MultimodalConfig,
    /// Full config, for resolving the configured `vision_model_provider`'s
    /// alias-specific runtime options (the `vision` override, endpoint URI,
    /// credentials) on the vision route. `None` on configless (test) paths,
    /// where the route falls back to the legacy factory.
    pub config: Option<&'a zeroclaw_config::schema::Config>,
    /// Agentic loop iteration cap.
    pub max_tool_iterations: usize,
    /// Lifecycle hooks; `None` when unconfigured.
    pub hooks: Option<&'a HookRunner>,
    /// Tools the policy denies (never invoked).
    pub excluded_tools: &'a [String],
    /// Tools exempt from call-dedup.
    pub dedup_exempt_tools: &'a [String],
    /// Activation set for on-demand (tool_search) MCP tools; shared so activated
    /// tools persist across iterations.
    pub activated_tools: Option<&'a Arc<Mutex<ActivatedToolSet>>>,
    /// Back-channel for the `model_switch` tool.
    pub model_switch_callback: Option<ModelSwitchCallback>,
    /// Loop-detection / ignore-tools / timing policy.
    pub pacing: &'a PacingConfig,
    /// Reject malformed tool-call protocol.
    pub strict_tool_parsing: bool,
    /// Allow concurrent tool execution.
    pub parallel_tools: bool,
    /// Truncation limit for tool outputs.
    pub max_tool_result_chars: usize,
    /// History-pruning token threshold.
    pub context_token_budget: usize,
    /// Tool-receipt tracer; `None` when receipts are off.
    pub receipt_generator: Option<&'a ReceiptGenerator>,
    /// Fine-grained loop behavior flags.
    pub knobs: &'a LoopKnobs,
}

/// The per-turn I/O wiring half of [`ResolvedAgentExecution::resolve`]'s input:
/// the borrowed sinks, channels, and policy handles a path holds for the turn.
/// A grouped input layer (not stored state); `resolve` spreads it into the bundle.
pub struct ResolvedIo<'a> {
    pub tools_registry: &'a [Box<dyn Tool>],
    pub observer: &'a dyn Observer,
    pub silent: bool,
    pub approval: Option<&'a ApprovalManager>,
    pub multimodal_config: &'a MultimodalConfig,
    /// Full config for vision-route provider-alias resolution; `None` on
    /// configless (test) paths. See [`ResolvedAgentExecution::config`].
    pub config: Option<&'a zeroclaw_config::schema::Config>,
    pub hooks: Option<&'a HookRunner>,
    pub activated_tools: Option<&'a Arc<Mutex<ActivatedToolSet>>>,
    pub model_switch_callback: Option<ModelSwitchCallback>,
    pub receipt_generator: Option<&'a ReceiptGenerator>,
}

/// The resolved per-agent runtime knobs half of [`ResolvedAgentExecution::resolve`]'s
/// input: the values derived from the agent's resolved config. A grouped input layer
/// (not stored state); `resolve` spreads it into the bundle.
pub struct ResolvedRuntimeKnobs<'a> {
    pub max_tool_iterations: usize,
    pub excluded_tools: &'a [String],
    pub dedup_exempt_tools: &'a [String],
    pub pacing: &'a PacingConfig,
    pub strict_tool_parsing: bool,
    pub parallel_tools: bool,
    pub max_tool_result_chars: usize,
    pub context_token_budget: usize,
    pub knobs: &'a LoopKnobs,
}

impl<'a> ResolvedAgentExecution<'a> {
    pub fn resolve(
        model_access: ResolvedModelAccess<'a>,
        io: ResolvedIo<'a>,
        runtime: ResolvedRuntimeKnobs<'a>,
    ) -> Self {
        Self {
            model_access,
            tools_registry: io.tools_registry,
            observer: io.observer,
            silent: io.silent,
            approval: io.approval,
            multimodal_config: io.multimodal_config,
            config: io.config,
            max_tool_iterations: runtime.max_tool_iterations,
            hooks: io.hooks,
            excluded_tools: runtime.excluded_tools,
            dedup_exempt_tools: runtime.dedup_exempt_tools,
            activated_tools: io.activated_tools,
            model_switch_callback: io.model_switch_callback,
            pacing: runtime.pacing,
            strict_tool_parsing: runtime.strict_tool_parsing,
            parallel_tools: runtime.parallel_tools,
            max_tool_result_chars: runtime.max_tool_result_chars,
            context_token_budget: runtime.context_token_budget,
            receipt_generator: io.receipt_generator,
            knobs: runtime.knobs,
        }
    }
}

#[cfg(test)]
mod run_model_query_tests {
    use super::ResolvedModelAccess;
    use crate::agent::cost::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{ChatRequest, ChatResponse};
    use zeroclaw_config::cost::CostTracker;
    use zeroclaw_config::schema::CostConfig;
    use zeroclaw_providers::traits::TokenUsage;
    use zeroclaw_providers::{ChatMessage, ModelProvider};

    /// Provider stub returning a fixed reply with caller-selected usage, so the
    /// accounting seam can prove both normal recording and fail-closed goals.
    struct UsageProvider {
        usage: Option<TokenUsage>,
        calls: AtomicUsize,
    }

    impl UsageProvider {
        fn with_usage(usage: Option<TokenUsage>) -> Self {
            Self {
                usage,
                calls: AtomicUsize::new(0),
            }
        }

        fn measured() -> Self {
            Self::with_usage(Some(TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(20),
                cached_input_tokens: None,
            }))
        }
    }

    async fn create_test_goal(task_id: String, agent: &str, token_limit: Option<u64>) {
        let control_plane = match crate::control_plane::control_plane() {
            Some(control_plane) => control_plane,
            None => {
                let sqlite_store = Arc::new(
                    crate::control_plane::SqliteTaskStore::new_in_memory()
                        .expect("test control-plane store"),
                );
                let store: Arc<dyn crate::control_plane::TaskRegistry> = sqlite_store.clone();
                let goal_store: Arc<dyn crate::control_plane::GoalTaskRegistry> = sqlite_store;
                let _ = crate::control_plane::init_control_plane(
                    crate::control_plane::ControlPlaneHandle {
                        store,
                        goal_store,
                        boot_id: "test-boot".into(),
                        recovered_goal_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
                        data_dir_lock: None,
                    },
                );
                crate::control_plane::control_plane().expect("test control plane initialized")
            }
        };

        control_plane
            .goal_store
            .create_goal(
                crate::control_plane::TaskRecord {
                    id: task_id.clone(),
                    kind: crate::control_plane::TaskKind::Goal,
                    agent: agent.into(),
                    status: crate::control_plane::TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: "test-boot".into(),
                    heartbeat_at: None,
                    depth: 0,
                    parent_id: None,
                    originator_route: None,
                    delivered: false,
                    idem_key: None,
                    principal_id: None,
                    started_at: "2026-07-21T00:00:00Z".into(),
                    finished_at: None,
                },
                crate::control_plane::GoalTaskRecord {
                    task_id,
                    objective: "test goal".into(),
                    effective_token_limit: token_limit,
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
                None,
            )
            .await
            .expect("test goal created");
    }

    #[async_trait]
    impl ModelProvider for UsageProvider {
        // Required by the trait but unused: `run_model_query` dispatches through
        // `chat`, which is overridden below to carry token usage.
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: self.usage.clone(),
                reasoning_content: None,
            })
        }
    }

    impl Attributable for UsageProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "usage-provider"
        }
    }

    fn access(provider: &UsageProvider) -> ResolvedModelAccess<'_> {
        ResolvedModelAccess {
            model_provider: provider,
            provider_name: "custom",
            model: "test-model",
            temperature: None,
        }
    }

    // Unscoped: no cost-tracking context on the task. Budget-gate allows (no-op),
    // usage recording is skipped, and the call still returns the provider's
    // response. This is the tests / CLI-without-cost shape.
    #[tokio::test]
    async fn run_model_query_returns_response_and_no_op_meters_when_unscoped() {
        let provider = UsageProvider::measured();
        let messages = [ChatMessage::user("hi")];
        let resp = access(&provider)
            .run_model_query(ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            })
            .await
            .expect("query ok");
        assert_eq!(resp.text.as_deref(), Some("ok"));
    }

    // Scoped: an accumulation-only cost context is present, so the seam records
    // the returned token usage into the turn accumulator (proving budget-gate ->
    // dispatch -> record are wired, not just the dispatch).
    #[tokio::test]
    async fn run_model_query_records_usage_under_cost_scope() {
        let provider = UsageProvider::measured();
        let messages = [ChatMessage::user("hi")];
        let ctx = ToolLoopCostTrackingContext::usage_only();
        let turn_usage = Arc::clone(&ctx.turn_usage);

        let resp = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                access(&provider)
                    .run_model_query(ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    })
                    .await
            })
            .await
            .expect("query ok");

        assert_eq!(resp.text.as_deref(), Some("ok"));
        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 100);
        assert_eq!(recorded.output_tokens, 20);
    }

    #[tokio::test]
    async fn run_model_query_rejects_missing_usage_for_a_goal_owned_turn() {
        // The max-iteration summary path uses this seam too, so successful
        // provider output without usage must fail before it can spend a goal
        // budget without a durable ledger record.
        let provider = UsageProvider::with_usage(None);
        let messages = [ChatMessage::user("hi")];
        let task_id = format!("goal-summary-usage-{}", uuid::Uuid::new_v4());
        create_test_goal(task_id.clone(), "agent-a", Some(1)).await;
        let goal = crate::control_plane::GoalAdmissionContext::new("agent-a")
            .with_goal_task_id(Some(task_id));
        let workspace = tempfile::tempdir().unwrap();
        let tracker = Arc::new(CostTracker::new(CostConfig::default(), workspace.path()).unwrap());
        let context = ToolLoopCostTrackingContext::new(tracker, Arc::new(Default::default()))
            .with_goal_admission_context(&goal);

        let result = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(context), async {
                access(&provider)
                    .run_model_query(ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    })
                    .await
            })
            .await;

        let error = result.expect_err("goal-owned provider usage must be required");
        assert!(
            format!("{error:#}").contains("goal accounting usage unavailable"),
            "unexpected goal-owned usage error: {error:#}"
        );
    }

    #[tokio::test]
    async fn run_model_query_rejects_trackerless_goal_before_provider_dispatch() {
        // A trackerless goal cannot charge a budget, so preflight must fail
        // before the provider sees the request rather than wasting a call that
        // can never be represented in the canonical usage ledger.
        let provider = UsageProvider::measured();
        let messages = [ChatMessage::user("hi")];
        let goal = crate::control_plane::GoalAdmissionContext::new("agent-a")
            .with_goal_task_id(Some("goal-trackerless".into()));
        let context = ToolLoopCostTrackingContext::usage_only().with_goal_admission_context(&goal);

        let error = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(context), async {
                access(&provider)
                    .run_model_query(ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    })
                    .await
            })
            .await
            .expect_err("trackerless goal must fail before provider dispatch");
        assert!(format!("{error:#}").contains("goal accounting tracker unavailable"));
        assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
    }
}
