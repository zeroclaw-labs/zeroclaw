//! The resolved per-agent execution context the turn engine requires.

use std::sync::{Arc, Mutex};

use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, SemanticEmptyTerminalCompletion};
use zeroclaw_config::schema::{MultimodalConfig, PacingConfig};
use zeroclaw_providers::{
    ModelProvider, ProviderDispatch, ReliableRejectedCompletionUsage, multimodal,
};

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
        crate::agent::turn::provider_call::enforce_tool_loop_budget()?;
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
        let result = ProviderDispatch::from_ref(self.model_provider)
            .chat_accounted(request, self.model, self.temperature)
            .await;

        match result {
            Ok(accounted) => {
                // Rejected attempts were billed, but did not supply the accepted
                // response. Record them first without updating context-window
                // fill; the accepted response below remains its sole source.
                if let Some(usage) = accounted.rejected_attempt_usage.as_ref() {
                    crate::agent::cost::record_rejected_tool_loop_cost_usage(
                        self.provider_name,
                        self.model,
                        usage,
                    );
                }
                // A terminal response without final text or tools was billed
                // but cannot be accepted. Keep it out of context-window fill
                // and successful response telemetry before returning its typed
                // cause to every one-shot caller.
                if accounted.response.is_semantically_empty_terminal() {
                    if let Some(usage) = accounted.response.usage.as_ref() {
                        crate::agent::cost::record_rejected_tool_loop_cost_usage(
                            self.provider_name,
                            self.model,
                            usage,
                        );
                    }
                    return Err(anyhow::Error::new(SemanticEmptyTerminalCompletion));
                }
                // Only a semantically valid result controls accepted context
                // usage and successful response telemetry.
                if let Some(usage) = accounted.response.usage.as_ref() {
                    crate::agent::cost::record_tool_loop_cost_usage(
                        self.provider_name,
                        self.model,
                        usage,
                    );
                }
                Ok(accounted.response)
            }
            Err(error) => {
                // Accounted dispatch carries rejected usage on failures in the
                // typed Reliable error chain. Keep the original error intact so
                // terminal-cause classification remains the provider's source
                // of truth.
                if let Some(usage) = error.chain().find_map(|cause| {
                    cause
                        .downcast_ref::<ReliableRejectedCompletionUsage>()
                        .map(|rejected| &rejected.usage)
                }) {
                    crate::agent::cost::record_rejected_tool_loop_cost_usage(
                        self.provider_name,
                        self.model,
                        usage,
                    );
                }
                Err(error)
            }
        }
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{
        ChatRequest, ChatResponse, SemanticEmptyTerminalCompletion, ToolCall,
    };
    use zeroclaw_providers::reliable::ReliableModelProvider;
    use zeroclaw_providers::traits::TokenUsage;
    use zeroclaw_providers::{ChatMessage, ModelProvider, ReliableRejectedCompletionUsage};

    /// Provider stub returning a fixed reply WITH token usage, so the seam's
    /// cost-recording path has something to record.
    struct UsageProvider;

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
            Ok(ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    cached_input_tokens: None,
                }),
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

    struct SemanticEmptyThenTextProvider {
        calls: Arc<AtomicUsize>,
        persist_empty: bool,
    }

    #[async_trait]
    impl ModelProvider for SemanticEmptyThenTextProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            let rejected = self.persist_empty || attempt == 0;
            Ok(ChatResponse {
                text: (!rejected).then(|| "recovered".to_string()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(80),
                    output_tokens: Some(if rejected { 5 } else { 7 }),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            })
        }
    }

    impl Attributable for SemanticEmptyThenTextProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "semantic-empty-test"
        }
    }

    struct DirectResponseProvider {
        response: ChatResponse,
    }

    #[async_trait]
    impl ModelProvider for DirectResponseProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(self.response.clone())
        }
    }

    impl Attributable for DirectResponseProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "direct-response-test"
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

    fn direct_access(provider: &DirectResponseProvider) -> ResolvedModelAccess<'_> {
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
        let provider = UsageProvider;
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
        let provider = UsageProvider;
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
    async fn run_model_query_separates_rejected_reliable_usage_from_context_fill() {
        let calls = Arc::new(AtomicUsize::new(0));
        let reliable = ReliableModelProvider::new(
            "reliable",
            vec![(
                "primary".to_string(),
                Box::new(SemanticEmptyThenTextProvider {
                    calls: Arc::clone(&calls),
                    persist_empty: false,
                }) as Box<dyn ModelProvider>,
            )],
            1,
            1,
        );
        let messages = [ChatMessage::user("hi")];
        let ctx = ToolLoopCostTrackingContext::usage_only();
        let turn_usage = Arc::clone(&ctx.turn_usage);

        let response = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                ResolvedModelAccess {
                    model_provider: &reliable,
                    provider_name: "reliable",
                    model: "test-model",
                    temperature: None,
                }
                .run_model_query(ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                })
                .await
            })
            .await
            .expect("the second Reliable attempt succeeds");

        assert_eq!(
            response.usage.expect("accepted usage").output_tokens,
            Some(7)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 160);
        assert_eq!(recorded.output_tokens, 12);
        assert_eq!(recorded.last_input_tokens, 80);
    }

    #[tokio::test]
    async fn run_model_query_records_rejected_reliable_usage_on_exhaustion() {
        let reliable = ReliableModelProvider::new(
            "reliable",
            vec![(
                "primary".to_string(),
                Box::new(SemanticEmptyThenTextProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    persist_empty: true,
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = [ChatMessage::user("hi")];
        let ctx = ToolLoopCostTrackingContext::usage_only();
        let turn_usage = Arc::clone(&ctx.turn_usage);

        let error = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                ResolvedModelAccess {
                    model_provider: &reliable,
                    provider_name: "reliable",
                    model: "test-model",
                    temperature: None,
                }
                .run_model_query(ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                })
                .await
                .expect_err("semantic-empty exhaustion must fail")
            })
            .await;

        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableRejectedCompletionUsage>())
        );
        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 80);
        assert_eq!(recorded.output_tokens, 5);
        assert_eq!(recorded.last_input_tokens, 0);
    }

    #[tokio::test]
    async fn run_model_query_rejects_direct_semantic_empty_text_before_accepted_accounting() {
        let messages = [ChatMessage::user("hi")];
        for text in ["", " \n\t", "<think>internal reasoning</think>"] {
            let provider = DirectResponseProvider {
                response: ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: Some(TokenUsage {
                        input_tokens: Some(80),
                        output_tokens: Some(5),
                        cached_input_tokens: None,
                    }),
                    reasoning_content: None,
                },
            };
            let ctx = ToolLoopCostTrackingContext::usage_only();
            let turn_usage = Arc::clone(&ctx.turn_usage);
            let error = TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(ctx), async {
                    direct_access(&provider)
                        .run_model_query(ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        })
                        .await
                        .expect_err("direct semantic-empty response must fail")
                })
                .await;

            assert!(
                error
                    .chain()
                    .any(|cause| cause.is::<SemanticEmptyTerminalCompletion>())
            );
            let recorded = *turn_usage.lock();
            assert_eq!(recorded.input_tokens, 80, "case {text:?}");
            assert_eq!(recorded.output_tokens, 5, "case {text:?}");
            assert_eq!(recorded.last_input_tokens, 0, "case {text:?}");
        }
    }

    #[tokio::test]
    async fn run_model_query_keeps_direct_tool_only_response_valid() {
        let provider = DirectResponseProvider {
            response: ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                    extra_content: None,
                }],
                usage: Some(TokenUsage {
                    input_tokens: Some(80),
                    output_tokens: Some(5),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            },
        };
        let messages = [ChatMessage::user("hi")];
        let ctx = ToolLoopCostTrackingContext::usage_only();
        let turn_usage = Arc::clone(&ctx.turn_usage);

        let response = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                direct_access(&provider)
                    .run_model_query(ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    })
                    .await
            })
            .await
            .expect("a direct tool-only response remains valid");

        assert!(response.has_tool_calls());
        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 80);
        assert_eq!(recorded.output_tokens, 5);
        assert_eq!(recorded.last_input_tokens, 80);
    }
}
