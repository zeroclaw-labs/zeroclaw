//! The resolved per-agent execution context the turn engine requires.

use std::sync::{Arc, Mutex};

use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, SemanticEmptyTerminalCompletion};
use zeroclaw_config::schema::{MultimodalConfig, PacingConfig, ResolvedContextLimits};
use zeroclaw_providers::dispatch::{AccountedAttempt, with_exact_dispatch_route};
use zeroclaw_providers::{ModelProvider, ProviderDispatch, multimodal};

use super::{ContextLimitsResolver, LoopKnobs, ModelSwitchCallback};
use crate::agent::cost::admit_goal_operation_if_scoped;
use crate::agent::tool_receipts::ReceiptGenerator;
use crate::approval::ApprovalManager;
use crate::hooks::HookRunner;
use crate::observability::Observer;
use crate::security::SecurityPolicy;
use crate::tools::ActivatedToolSet;
use crate::tools::scoped::ScopedToolRegistry;

/// The resolved model binding: which provider, model, and temperature a turn
/// uses. The base layer any LLM call needs; [`ResolvedAgentExecution`] composes
/// it. Field names mirror the engine's former flat fields so the loop body is
/// unchanged after destructuring.
pub struct ResolvedModelAccess<'a> {
    pub model_provider: &'a dyn ModelProvider,
    /// Provider profile that actually serves this request.
    pub provider_name: &'a str,
    /// Model that actually serves this request.
    pub model: &'a str,
    /// Selector sent to `model_provider`. Routed providers may require a
    /// `hint:<name>` selector even though `model` records the resolved model.
    pub dispatch_model: &'a str,
    pub temperature: Option<f64>,
}

/// Owned per-attempt usage summary for a finished `run_model_query` call.
/// The seam settles costs but owns no event sink, so callers that do (the
/// max-iteration summary path) project these as `TurnEvent::Usage` to keep
/// the gateway ledger complete.
#[derive(Debug, Clone)]
pub struct SettledAttemptSummary {
    pub provider_ref: String,
    pub model: String,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub accepted: bool,
    /// Display-only estimate of the summary request's input tokens, set by
    /// the caller when the provider omitted `input_tokens`. Rejected attempts
    /// leave this `None`; accounting reads only `input_tokens`.
    pub estimated_input_tokens: Option<u64>,
}

/// Append summaries for the billable subset of `attempts` as rejected
/// (`accepted: false`) leaves. Costs are computed without touching the
/// accumulators — the caller already settled them.
fn extend_rejected_summaries(out: &mut Vec<SettledAttemptSummary>, attempts: &[AccountedAttempt]) {
    for billable in crate::agent::cost::billable_provider_attempts(attempts) {
        out.push(SettledAttemptSummary {
            provider_ref: billable.attempt.provider_ref().to_string(),
            model: billable.attempt.model().to_string(),
            input_tokens: billable.usage.input_tokens,
            cached_input_tokens: billable.usage.cached_input_tokens,
            output_tokens: billable.usage.output_tokens,
            cost_usd: crate::agent::cost::compute_cost_usd(
                billable.attempt.provider_ref(),
                billable.attempt.model(),
                billable.usage,
            ),
            accepted: false,
            estimated_input_tokens: None,
        });
    }
}

impl ResolvedModelAccess<'_> {
    pub async fn run_model_query(
        &self,
        request: ChatRequest<'_>,
        settled_out: &mut Vec<SettledAttemptSummary>,
    ) -> anyhow::Result<ChatResponse> {
        // Fail closed before spending a provider call when the enclosing turn's
        // cost budget is already exhausted. No-op when unscoped.
        crate::agent::turn::provider_call::enforce_tool_loop_budget().map_err(|error| {
            if crate::agent::cost::goal_operation_accounting_is_scoped() {
                // This one-shot query has not begun any local tool batch or
                // admitted a provider operation at this pre-dispatch boundary.
                zeroclaw_goal::accounting::GoalOrdinarySpendRefusal::new(error, true).into()
            } else {
                error
            }
        })?;
        // This one-shot seam does not run the full multimodal preparation
        // (`prepare_messages_for_provider`): callers that want images in the
        // request normalize before calling, and the max-iteration graceful
        // summary does. As a fail-closed backstop for every other caller,
        // loadable audio markers are replaced with a placeholder here, and
        // image references that are not inline `data:` URIs are dropped the
        // same way: a path or URL marker in a legacy body becomes a
        // placeholder, while a declared carrier keeps its body verbatim and
        // loses only the undeliverable attachments from its declared list.
        // Both helpers borrow the input untouched when clean.
        let ChatRequest {
            messages,
            tools,
            thinking,
        } = request;
        let sanitized = multimodal::sanitize_audio_markers(messages);
        let sanitized = multimodal::sanitize_image_markers(&sanitized);
        let request = ChatRequest {
            messages: &sanitized,
            tools,
            thinking,
        };
        let foreground = crate::goal_accounting::GoalForegroundContext::current();
        if foreground.as_ref().is_some_and(|context| {
            context.token().is_cancelled()
                || context
                    .deadline()
                    .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        }) {
            return Err(super::outcome::ToolLoopCancelled.into());
        }
        admit_goal_operation_if_scoped(self.provider_name, self.model).await?;
        let dispatcher = ProviderDispatch::from_ref(self.model_provider);
        let scope = if crate::agent::cost::goal_operation_accounting_is_scoped() {
            zeroclaw_providers::dispatch::AccountedChatScope::new_with_string_usage()
        } else {
            zeroclaw_providers::dispatch::AccountedChatScope::new()
        };
        // The collector outlives cancellation of the physical request, so its
        // immutable unknown/lower-bound report still reaches settlement below.
        let request_future = scope
            // The route identity records the model that actually serves this
            // request, while the dispatch call keeps the provider-facing
            // selector: a routed provider may need `hint:<name>` even though
            // `model` already records the resolved model behind that hint.
            .scope(with_exact_dispatch_route(
                self.provider_name.to_string(),
                self.model.to_string(),
                dispatcher.chat(request, self.dispatch_model, self.temperature),
            ));
        let result = if let Some(context) = &foreground {
            tokio::select! {
                biased;
                () = context.token().cancelled() => Err(super::outcome::ToolLoopCancelled.into()),
                result = request_future => result,
            }
        } else {
            request_future.await
        };
        if result.is_ok() {
            scope.mark_logical_success();
        }
        let accounting = scope.take();

        let attempts = accounting.attempts();
        let goal_scoped = crate::agent::cost::goal_operation_accounting_is_scoped();

        let accepted_route = accounting.accepted_route().cloned();
        let (served_provider, served_model) = accepted_route
            .as_ref()
            .map(|route| (route.provider_ref().to_string(), route.model().to_string()))
            .unwrap_or_else(|| (self.provider_name.to_string(), self.model.to_string()));
        match result {
            Ok(response) => {
                // A terminal response without final text or tools was billed
                // but cannot be accepted. Keep it out of context-window fill
                // and successful response telemetry before returning its typed
                // cause to every one-shot caller.
                if response.is_semantically_empty_terminal() {
                    let error = anyhow::Error::new(SemanticEmptyTerminalCompletion);
                    let settlement =
                        crate::agent::cost::settle_provider_attempts(attempts, None).await;
                    extend_rejected_summaries(settled_out, attempts);
                    return Err(match settlement {
                        Ok(()) => error,
                        Err(secondary) => zeroclaw_goal::accounting::GoalAccountingFailure::preserving_primary_error(error, secondary),
                    });
                }
                let settlement = if goal_scoped {
                    crate::agent::cost::settle_provider_attempts(
                        attempts,
                        Some(attempts.len().saturating_sub(1)),
                    )
                    .await
                } else {
                    crate::agent::cost::settle_provider_attempts(
                        &attempts[..attempts.len().saturating_sub(1)],
                        None,
                    )
                    .await
                };
                if let Err(error) = settlement {
                    extend_rejected_summaries(settled_out, attempts);
                    return Err(error);
                }
                zeroclaw_providers::dispatch::commit_accepted_provider_route(accepted_route);
                extend_rejected_summaries(
                    settled_out,
                    &attempts[..attempts.len().saturating_sub(1)],
                );
                // Only a semantically valid result controls accepted context
                // usage and successful response telemetry.
                let accepted_cost_usd = if goal_scoped {
                    response.usage.as_ref().and_then(|usage| {
                        crate::agent::cost::compute_cost_usd(&served_provider, &served_model, usage)
                    })
                } else {
                    response.usage.as_ref().and_then(|usage| {
                        crate::agent::cost::record_tool_loop_cost_usage(
                            &served_provider,
                            &served_model,
                            usage,
                        )
                        .map(|(_, cost_usd)| cost_usd)
                    })
                };
                // The accepted leaf is always projected (even usage-less, so
                // terminal identity stays coherent); the caller emits it with
                // `accepted: true`.
                settled_out.push(SettledAttemptSummary {
                    provider_ref: served_provider,
                    model: served_model,
                    input_tokens: response.usage.as_ref().and_then(|usage| usage.input_tokens),
                    cached_input_tokens: response
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.cached_input_tokens),
                    output_tokens: response
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.output_tokens),
                    cost_usd: accepted_cost_usd,
                    accepted: true,
                    // The caller (max_iter) fills this from the summary
                    // request's message history when the provider omitted
                    // `input_tokens`, so the context meter keeps its numerator
                    // after the final-summary event.
                    estimated_input_tokens: None,
                });
                Ok(response)
            }
            Err(error) => {
                let settlement = crate::agent::cost::settle_provider_attempts(attempts, None).await;
                extend_rejected_summaries(settled_out, attempts);
                Err(match settlement {
                    Ok(()) => error,
                    Err(secondary) => {
                        zeroclaw_goal::accounting::GoalAccountingFailure::preserving_primary_error(
                            error, secondary,
                        )
                    }
                })
            }
        }
    }
}

pub struct ResolvedAgentExecution<'a> {
    /// Provider + model + temperature.
    pub model_access: ResolvedModelAccess<'a>,
    /// The tools available this turn, sealed as a [`ScopedToolRegistry`] so the
    /// engine can only be handed a registry minted by `assemble()` (the seal).
    pub tools_registry: &'a ScopedToolRegistry,
    /// Telemetry/audit sink.
    pub observer: &'a dyn Observer,
    /// Suppress stderr output (subagents/reviews run silent).
    pub silent: bool,
    /// Approval policy + back-channel; `None` for paths that never prompt.
    pub approval: Option<&'a ApprovalManager>,
    /// The agent's filesystem policy, applied by the no-vision image-marker
    /// gate so a local marker counts as resolvable only when the agent's own
    /// file tools could read it. `None` on configless (test) paths, where the
    /// gate fails closed to a degrade. The gate lives in
    /// `crate::agent::turn::vision_route::resolve_vision_provider`.
    pub security: Option<&'a SecurityPolicy>,
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
    /// Capacity and proactive-trim budget resolved together for the selected
    /// route at the turn boundary.
    pub context_limits: ResolvedContextLimits,
    /// The turn engine's single authority for per-call `(provider, model)`
    /// limits. Daemon-backed agents pass a closure reading the shared live
    /// `Config`; configless (test) paths leave it `None` and the loop uses
    /// `context_limits`. Prevents recomputing capacity from a stale config
    /// snapshot when the active route or live config changes mid-turn.
    pub context_limits_resolver: Option<ContextLimitsResolver>,
    /// Tool-receipt tracer; `None` when receipts are off.
    pub receipt_generator: Option<&'a ReceiptGenerator>,
    /// Fine-grained loop behavior flags.
    pub knobs: &'a LoopKnobs,
}

/// The per-turn I/O wiring half of [`ResolvedAgentExecution::resolve`]'s input:
/// the borrowed sinks, channels, and policy handles a path holds for the turn.
/// A grouped input layer (not stored state); `resolve` spreads it into the bundle.
pub struct ResolvedIo<'a> {
    pub tools_registry: &'a ScopedToolRegistry,
    pub observer: &'a dyn Observer,
    pub silent: bool,
    pub approval: Option<&'a ApprovalManager>,
    /// Filesystem policy for the no-vision image-marker gate; `None` on
    /// configless (test) paths. See [`ResolvedAgentExecution::security`].
    pub security: Option<&'a SecurityPolicy>,
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
    pub context_limits: ResolvedContextLimits,
    /// Single live limits authority; see [`ResolvedAgentExecution::context_limits_resolver`].
    pub context_limits_resolver: Option<ContextLimitsResolver>,
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
            security: io.security,
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
            context_limits: runtime.context_limits,
            context_limits_resolver: runtime.context_limits_resolver,
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
                    cache_creation_input_tokens: None,
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
                    cache_creation_input_tokens: None,
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
            dispatch_model: "test-model",
            temperature: None,
        }
    }

    fn direct_access(provider: &DirectResponseProvider) -> ResolvedModelAccess<'_> {
        ResolvedModelAccess {
            model_provider: provider,
            provider_name: "custom",
            model: "test-model",
            dispatch_model: "test-model",
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
            .run_model_query(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                &mut Vec::new(),
            )
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
                    .run_model_query(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        &mut Vec::new(),
                    )
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

        let mut summaries = Vec::new();
        let response = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                ResolvedModelAccess {
                    model_provider: &reliable,
                    provider_name: "reliable",
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
        // The summary projects exactly what the gateway ledger needs: the
        // rejected first attempt plus the accepted second attempt.
        assert_eq!(summaries.len(), 2);
        assert!(!summaries[0].accepted);
        assert_eq!(summaries[0].input_tokens, Some(80));
        assert_eq!(summaries[0].output_tokens, Some(5));
        assert!(summaries[1].accepted);
        assert_eq!(summaries[1].input_tokens, Some(80));
        assert_eq!(summaries[1].output_tokens, Some(7));
    }

    struct CapturedGoalSettlement(
        Arc<parking_lot::Mutex<Vec<zeroclaw_goal::accounting::GoalOperationSettlement>>>,
    );

    struct RefusedQueryAccounting {
        admits: Arc<AtomicUsize>,
        settlements: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for RefusedQueryAccounting {
        async fn admit(
            &self,
            _: zeroclaw_goal::accounting::GoalOperationRequest,
        ) -> anyhow::Result<()> {
            self.admits.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("refused query unexpectedly reached admission")
        }

        async fn settle(
            &self,
            _: zeroclaw_goal::accounting::GoalOperationSettlement,
        ) -> anyhow::Result<()> {
            self.settlements.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("refused query unexpectedly reached settlement")
        }

        async fn pricing_complete(&self) -> anyhow::Result<bool> {
            anyhow::bail!("refused query unexpectedly queried pricing completeness")
        }
    }

    #[tokio::test]
    async fn run_model_query_qualifies_only_goal_pre_admission_spend_refusal() {
        use zeroclaw_api::turn_stop::{TurnStopCode, turn_stop};
        use zeroclaw_goal::accounting::{GOAL_OPERATION_ACCOUNTING, GoalOrdinarySpendRefusal};

        for goal_scoped in [false, true] {
            let directory = tempfile::TempDir::new().unwrap();
            let tracker = Arc::new(
                crate::cost::CostTracker::new(
                    zeroclaw_config::schema::CostConfig {
                        enabled: true,
                        daily_limit_usd: 1.0,
                        monthly_limit_usd: 100.0,
                        ..Default::default()
                    },
                    directory.path(),
                )
                .unwrap(),
            );
            tracker
                .record_usage(crate::cost::types::TokenUsage::new(
                    "seed", 1_000_000, 0, 0, 2.0, 0.0, 0.0,
                ))
                .unwrap();
            let ledger_path = directory.path().join("state/costs.jsonl");
            let before = std::fs::read(&ledger_path).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let provider = SemanticEmptyThenTextProvider {
                calls: Arc::clone(&calls),
                persist_empty: false,
            };
            let admits = Arc::new(AtomicUsize::new(0));
            let settlements = Arc::new(AtomicUsize::new(0));
            let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
                Arc::new(RefusedQueryAccounting {
                    admits: Arc::clone(&admits),
                    settlements: Arc::clone(&settlements),
                });
            let context = ToolLoopCostTrackingContext::new(
                tracker,
                Arc::new(std::collections::HashMap::new()),
            );
            let messages = [ChatMessage::user("do not dispatch")];
            let mut summaries = Vec::new();
            let error = GOAL_OPERATION_ACCOUNTING
                .scope(
                    goal_scoped.then_some(accountant),
                    TOOL_LOOP_COST_TRACKING_CONTEXT.scope(Some(context), async {
                        ResolvedModelAccess {
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
                    }),
                )
                .await;

            assert_eq!(
                turn_stop(&error).unwrap().code,
                TurnStopCode::BudgetExhausted
            );
            let marker = error.downcast_ref::<GoalOrdinarySpendRefusal>();
            assert_eq!(marker.is_some(), goal_scoped);
            if let Some(marker) = marker {
                assert!(marker.clean_tool_phase());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(admits.load(Ordering::SeqCst), 0);
            assert_eq!(settlements.load(Ordering::SeqCst), 0);
            assert!(summaries.is_empty());
            assert_eq!(std::fs::read(ledger_path).unwrap(), before);
        }
    }

    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for CapturedGoalSettlement {
        async fn admit(
            &self,
            _request: zeroclaw_goal::accounting::GoalOperationRequest,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn settle(
            &self,
            settlement: zeroclaw_goal::accounting::GoalOperationSettlement,
        ) -> anyhow::Result<()> {
            self.0.lock().push(settlement);
            Ok(())
        }

        async fn pricing_complete(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    struct FailingGoalSettlement;
    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for FailingGoalSettlement {
        async fn admit(
            &self,
            _: zeroclaw_goal::accounting::GoalOperationRequest,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn settle(
            &self,
            _: zeroclaw_goal::accounting::GoalOperationSettlement,
        ) -> anyhow::Result<()> {
            Err(zeroclaw_goal::accounting::GoalAccountingFailure::settled(
                zeroclaw_goal::GoalAccountingState::OutcomeUnknown,
                &["secondary accounting detail".into()],
            )
            .into())
        }
        async fn pricing_complete(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn settlement_failure_preserves_semantic_empty_and_rejected_usage() {
        let provider = DirectResponseProvider {
            response: ChatResponse {
                text: Some(" ".into()),
                tool_calls: vec![],
                usage: Some(TokenUsage {
                    input_tokens: Some(8),
                    output_tokens: Some(3),
                    cached_input_tokens: None,
                    cache_creation_input_tokens: None,
                }),
                reasoning_content: None,
            },
        };
        let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(FailingGoalSettlement);
        let messages = [ChatMessage::user("hi")];
        let mut summaries = Vec::new();
        let error = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(Some(accountant), async {
                direct_access(&provider)
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
            })
            .await;
        assert!(
            error
                .downcast_ref::<SemanticEmptyTerminalCompletion>()
                .is_some()
        );
        assert_eq!(
            error
                .downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>()
                .unwrap()
                .accounting_state(),
            zeroclaw_goal::GoalAccountingState::OutcomeUnknown
        );
        assert!(format!("{error:#}").contains("secondary accounting detail"));
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].accepted);
        assert_eq!(summaries[0].input_tokens, Some(8));
        assert_eq!(summaries[0].output_tokens, Some(3));
    }

    #[tokio::test]
    async fn settlement_failure_rejects_valid_response_summary() {
        let provider = UsageProvider;
        let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(FailingGoalSettlement);
        let messages = [ChatMessage::user("hi")];
        let mut summaries = Vec::new();
        let error = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(Some(accountant), async {
                access(&provider)
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
            })
            .await;
        assert!(
            error
                .downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>()
                .is_some()
        );
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].accepted);
        assert_eq!(summaries[0].input_tokens, Some(100));
    }

    #[tokio::test]
    async fn settlement_failure_preserves_reliable_exhaustion_and_rejected_summary() {
        let calls = Arc::new(AtomicUsize::new(0));
        let reliable = ReliableModelProvider::new(
            "reliable",
            vec![(
                "primary".to_owned(),
                Box::new(SemanticEmptyThenTextProvider {
                    calls: Arc::clone(&calls),
                    persist_empty: true,
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(FailingGoalSettlement);
        let messages = [ChatMessage::user("hi")];
        let mut summaries = Vec::new();
        let error = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(Some(accountant), async {
                ResolvedModelAccess {
                    model_provider: &reliable,
                    provider_name: "reliable",
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
            })
            .await;
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableRejectedCompletionUsage>())
        );
        assert!(
            error
                .downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>()
                .is_some()
        );
        assert!(format!("{error:#}").contains("secondary accounting detail"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].provider_ref, "primary");
        assert!(!summaries[0].accepted);
        assert_eq!(
            (summaries[0].input_tokens, summaries[0].output_tokens),
            (Some(80), Some(5))
        );
    }

    #[tokio::test]
    async fn goal_scoped_model_query_projects_usage_like_ordinary_without_ordinary_ledger() {
        let ordinary_calls = Arc::new(AtomicUsize::new(0));
        let ordinary = ReliableModelProvider::new(
            "reliable",
            vec![(
                "primary".to_string(),
                Box::new(SemanticEmptyThenTextProvider {
                    calls: Arc::clone(&ordinary_calls),
                    persist_empty: false,
                }) as Box<dyn ModelProvider>,
            )],
            1,
            1,
        );
        let goal_calls = Arc::new(AtomicUsize::new(0));
        let goal = ReliableModelProvider::new(
            "reliable",
            vec![(
                "primary".to_string(),
                Box::new(SemanticEmptyThenTextProvider {
                    calls: Arc::clone(&goal_calls),
                    persist_empty: false,
                }) as Box<dyn ModelProvider>,
            )],
            1,
            1,
        );
        let messages = [ChatMessage::user("hi")];
        let make_context = |root: &std::path::Path| {
            let tracker = Arc::new(
                crate::cost::CostTracker::new(
                    zeroclaw_config::schema::CostConfig {
                        enabled: true,
                        ..Default::default()
                    },
                    root,
                )
                .expect("cost tracker"),
            );
            let pricing = Arc::new(std::collections::HashMap::from([(
                "primary".to_string(),
                std::collections::HashMap::from([
                    ("test-model.input".to_string(), 1.0),
                    ("test-model.output".to_string(), 2.0),
                ]),
            )]));
            ToolLoopCostTrackingContext::new(tracker, pricing)
        };
        let ordinary_root = tempfile::TempDir::new().expect("ordinary temp dir");
        let ordinary_context = make_context(ordinary_root.path());
        let ordinary_usage = Arc::clone(&ordinary_context.turn_usage);
        let mut ordinary_summaries = Vec::new();
        let ordinary_response = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ordinary_context), async {
                ResolvedModelAccess {
                    model_provider: &ordinary,
                    provider_name: "reliable",
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
                    &mut ordinary_summaries,
                )
                .await
            })
            .await
            .expect("ordinary query succeeds");

        let goal_root = tempfile::TempDir::new().expect("Goal temp dir");
        let goal_context = make_context(goal_root.path());
        let goal_usage = Arc::clone(&goal_context.turn_usage);
        let settlements = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(CapturedGoalSettlement(Arc::clone(&settlements)));
        let mut goal_summaries = Vec::new();
        let goal_response = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(Some(accountant), async {
                TOOL_LOOP_COST_TRACKING_CONTEXT
                    .scope(Some(goal_context), async {
                        ResolvedModelAccess {
                            model_provider: &goal,
                            provider_name: "reliable",
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
                            &mut goal_summaries,
                        )
                        .await
                    })
                    .await
            })
            .await
            .expect("Goal-scoped query succeeds");

        assert_eq!(ordinary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(goal_calls.load(Ordering::SeqCst), 2);
        assert_eq!(ordinary_response.text, goal_response.text);
        assert_eq!(ordinary_summaries.len(), 2);
        assert_eq!(goal_summaries.len(), ordinary_summaries.len());
        for (index, (goal, ordinary)) in goal_summaries.iter().zip(&ordinary_summaries).enumerate()
        {
            assert_eq!(goal.provider_ref, ordinary.provider_ref);
            assert_eq!(goal.model, ordinary.model);
            assert_eq!(goal.provider_ref, "primary");
            assert_eq!(goal.model, "test-model");
            assert_eq!(goal.input_tokens, ordinary.input_tokens);
            assert_eq!(goal.cached_input_tokens, ordinary.cached_input_tokens);
            assert_eq!(goal.output_tokens, ordinary.output_tokens);
            assert_eq!(goal.cost_usd, ordinary.cost_usd);
            assert_eq!(goal.accepted, ordinary.accepted);
            assert_eq!(goal.accepted, index == 1);
            assert_eq!(goal.input_tokens, Some(80));
            assert_eq!(goal.output_tokens, Some(if index == 0 { 5 } else { 7 }));
            assert!(goal.cost_usd.is_some_and(|cost| cost > 0.0));
        }
        // TurnUsage deliberately has no PartialEq; compare its owned facts.
        let goal_usage = *goal_usage.lock();
        let ordinary_usage = *ordinary_usage.lock();
        assert_eq!(goal_usage.input_tokens, ordinary_usage.input_tokens);
        assert_eq!(goal_usage.output_tokens, ordinary_usage.output_tokens);
        assert_eq!(goal_usage.cost_usd, ordinary_usage.cost_usd);
        assert_eq!(
            goal_usage.last_input_tokens,
            ordinary_usage.last_input_tokens
        );
        assert_eq!(goal_usage.input_tokens, 160);
        assert_eq!(goal_usage.output_tokens, 12);
        assert_eq!(goal_usage.last_input_tokens, 80);
        let settlements = settlements.lock();
        assert_eq!(settlements.len(), 1);
        assert_eq!(
            settlements[0].accounting_state,
            zeroclaw_goal::task::GoalAccountingState::Complete
        );
        assert_eq!(settlements[0].events.len(), 2);
        for (event, summary) in settlements[0].events.iter().zip(&goal_summaries) {
            assert_eq!(event.provider_ref, summary.provider_ref);
            assert_eq!(event.model, summary.model);
            assert_eq!(event.usage.input_tokens, summary.input_tokens);
            assert_eq!(event.usage.output_tokens, summary.output_tokens);
            // The summary keeps the provider's absent cache count; the canonical
            // Goal event records a valid absent cache partition as explicit zeros.
            assert_eq!(summary.cached_input_tokens, None);
            assert_eq!(event.usage.cached_input_tokens, Some(0));
            assert_eq!(event.usage.cache_creation_input_tokens, Some(0));
            assert!(event.priceable);
        }
        assert!(ordinary_root.path().join("state/costs.jsonl").exists());
        assert!(!goal_root.path().join("state/costs.jsonl").exists());
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

        let mut summaries = Vec::new();
        let error = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                ResolvedModelAccess {
                    model_provider: &reliable,
                    provider_name: "reliable",
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
        // Exhaustion still yields a projectable rejected summary so terminal
        // exits keep the ledger complete.
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].accepted);
        assert_eq!(summaries[0].input_tokens, Some(80));
        assert_eq!(summaries[0].output_tokens, Some(5));
    }

    #[tokio::test]
    async fn run_model_query_rejected_cache_write_projection_matches_tracker_record() {
        let workspace = tempfile::TempDir::new().unwrap();
        let tracker = Arc::new(
            crate::cost::CostTracker::new(
                zeroclaw_config::schema::CostConfig::default(),
                workspace.path(),
            )
            .unwrap(),
        );
        let pricing = Arc::new(std::collections::HashMap::from([(
            "custom".to_string(),
            std::collections::HashMap::from([
                ("test-model.input".to_string(), 0.27),
                ("test-model.cached_input".to_string(), 0.027),
                ("test-model.cache_write".to_string(), 0.54),
                ("test-model.output".to_string(), 1.10),
            ]),
        )]));
        let ctx = ToolLoopCostTrackingContext::new(tracker, pricing);
        // A semantically empty terminal response is billed but never
        // accepted: exactly the rejected-attempt shape under test.
        let provider = DirectResponseProvider {
            response: ChatResponse {
                text: Some(String::new()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(5_000),
                    output_tokens: Some(200),
                    cached_input_tokens: Some(4_000),
                    cache_creation_input_tokens: Some(1_000),
                }),
                reasoning_content: None,
            },
        };
        let messages = [ChatMessage::user("hi")];
        let mut summaries = Vec::new();
        let error = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                direct_access(&provider)
                    .run_model_query(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        &mut summaries,
                    )
                    .await
                    .expect_err("semantic-empty direct response must fail")
            })
            .await;

        assert!(
            error
                .chain()
                .any(|cause| cause.is::<SemanticEmptyTerminalCompletion>())
        );
        // The rejected summary's projected cost must equal what settlement
        // actually wrote to the tracker ledger for the same attempt.
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].accepted);
        assert_eq!(summaries[0].input_tokens, Some(5_000));
        assert_eq!(summaries[0].cached_input_tokens, Some(4_000));
        assert_eq!(summaries[0].output_tokens, Some(200));
        let projected = summaries[0].cost_usd.expect("rejected cost is priced");
        let stored = std::fs::read_to_string(workspace.path().join("state").join("costs.jsonl"))
            .expect("costs.jsonl should be written");
        let record: zeroclaw_config::cost::types::CostRecord =
            serde_json::from_str(stored.lines().next().expect("one record")).unwrap();
        assert_eq!(record.usage.cache_creation_input_tokens, 1_000);
        assert!(
            (projected - record.usage.cost_usd).abs() < 1e-12,
            "rejected projection must match the settled tracker record"
        );

        // Both must price the write band at the write premium.
        let expected = (1_000.0 * 0.54 + 4_000.0 * 0.027 + 200.0 * 1.10) / 1_000_000.0;
        assert!(
            (projected - expected).abs() < 1e-12,
            "projection must price cache writes at the write rate"
        );
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
                        cache_creation_input_tokens: None,
                    }),
                    reasoning_content: None,
                },
            };
            let ctx = ToolLoopCostTrackingContext::usage_only();
            let turn_usage = Arc::clone(&ctx.turn_usage);
            let error = TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(ctx), async {
                    direct_access(&provider)
                        .run_model_query(
                            ChatRequest {
                                messages: &messages,
                                tools: None,
                                thinking: None,
                            },
                            &mut Vec::new(),
                        )
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
                    cache_creation_input_tokens: None,
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
                    .run_model_query(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        &mut Vec::new(),
                    )
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
