//! The resolved per-agent execution context the turn engine requires.

use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, SemanticEmptyTerminalCompletion};
use zeroclaw_config::schema::{MultimodalConfig, PacingConfig, ResolvedContextLimits};
use zeroclaw_providers::dispatch::{AccountedAttempt, with_exact_dispatch_route};
use zeroclaw_providers::{ModelProvider, ProviderDispatch, multimodal};

use super::{ContextLimitsResolver, LoopKnobs, ModelSwitchCallback};
use crate::agent::cost::{
    admit_goal_operation_if_scoped, release_unstarted_goal_operation_if_scoped,
};
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
            cost_usd: crate::agent::cost::compute_attempt_cost_usd(
                billable.attempt,
                billable.usage,
            ),
            accepted: false,
            estimated_input_tokens: None,
        });
    }
}

/// The existing turn deadline and cancellation signal, shared by summary
/// preparation and its provider call. No new timeout budget is introduced.
#[derive(Clone, Copy)]
pub(super) struct ModelQueryControl<'a> {
    pub deadline: Option<Instant>,
    pub cancellation_token: Option<&'a CancellationToken>,
    pub foreground_token: Option<&'a CancellationToken>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ModelQueryInterruption {
    Cancelled,
    TimedOut,
}

impl std::fmt::Display for ModelQueryInterruption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Cancelled => "model query cancelled",
            Self::TimedOut => "model query timed out",
        })
    }
}

impl std::error::Error for ModelQueryInterruption {}

impl<'a> ModelQueryControl<'a> {
    pub(super) fn with_foreground(
        mut self,
        foreground: Option<&'a crate::goal_accounting::GoalForegroundContext>,
    ) -> Self {
        if let Some(foreground) = foreground {
            if let Some(deadline) = foreground.deadline() {
                self.deadline = Some(self.deadline.map_or(deadline, |local| local.min(deadline)));
            }
            self.foreground_token = Some(foreground.token());
        }
        self
    }

    fn current(self) -> Option<ModelQueryInterruption> {
        if [self.cancellation_token, self.foreground_token]
            .into_iter()
            .flatten()
            .any(CancellationToken::is_cancelled)
        {
            Some(ModelQueryInterruption::Cancelled)
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Some(ModelQueryInterruption::TimedOut)
        } else {
            None
        }
    }

    pub(super) async fn interrupted(self) -> ModelQueryInterruption {
        async fn cancelled(token: Option<&CancellationToken>) {
            match token {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        }
        async fn expired(deadline: Option<Instant>) {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        }
        tokio::select! {
            biased;
            () = cancelled(self.cancellation_token) => ModelQueryInterruption::Cancelled,
            () = cancelled(self.foreground_token) => ModelQueryInterruption::Cancelled,
            () = expired(self.deadline) => ModelQueryInterruption::TimedOut,
        }
    }
}

impl ResolvedModelAccess<'_> {
    pub async fn run_model_query(
        &self,
        request: ChatRequest<'_>,
        settled_out: &mut Vec<SettledAttemptSummary>,
    ) -> anyhow::Result<ChatResponse> {
        self.run_model_query_controlled(
            request,
            settled_out,
            ModelQueryControl {
                deadline: None,
                cancellation_token: None,
                foreground_token: None,
            },
        )
        .await
    }

    pub(super) async fn run_model_query_controlled(
        &self,
        request: ChatRequest<'_>,
        settled_out: &mut Vec<SettledAttemptSummary>,
        control: ModelQueryControl<'_>,
    ) -> anyhow::Result<ChatResponse> {
        let foreground = crate::goal_accounting::GoalForegroundContext::current();
        let control = control.with_foreground(foreground.as_ref());
        if let Some(interruption) = control.current() {
            return Err(interruption.into());
        }
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
        if let Some(interruption) = control.current() {
            return Err(interruption.into());
        }
        // Admission may commit a durable pending marker. Do not race and drop
        // that future: after it completes, release it if no dispatch began.
        let _goal_admission =
            admit_goal_operation_if_scoped(self.provider_name, self.model).await?;
        if let Some(interruption) = control.current() {
            release_unstarted_goal_operation_if_scoped()
                .await
                .map_err(|error| {
                    crate::agent::cost::with_visible_context(
                        error,
                        &format!("while handling {interruption}"),
                    )
                })?;
            return Err(interruption.into());
        }
        let mut dispatch_start = if crate::agent::cost::goal_operation_accounting_is_scoped() {
            let start = match crate::agent::cost::begin_goal_dispatch_if_scoped().await {
                Ok(start) => start,
                Err(error) => {
                    let release = release_unstarted_goal_operation_if_scoped().await;
                    return Err(match release {
                        Ok(()) => error,
                        Err(release_error) => {
                            crate::agent::cost::with_settlement_failure(error, release_error)
                        }
                    });
                }
            };
            if let Some(interruption) = control.current() {
                drop(start);
                release_unstarted_goal_operation_if_scoped()
                    .await
                    .map_err(|error| {
                        crate::agent::cost::with_visible_context(
                            error,
                            &format!("while handling {interruption}"),
                        )
                    })?;
                return Err(interruption.into());
            }
            start
        } else {
            None
        };
        let dispatcher = ProviderDispatch::from_ref(self.model_provider);
        let scope = crate::agent::cost::new_provider_attempt_scope();
        let mut dispatch_polled = false;
        let outcome = {
            // Drop the provider future before taking the scope so an in-flight
            // attempt is finalized as outcome-unknown on interruption.
            let provider_future = scope.scope(with_exact_dispatch_route(
                self.provider_name.to_string(),
                self.model.to_string(),
                dispatcher.chat(request, self.dispatch_model, self.temperature),
            ));
            tokio::pin!(provider_future);
            tokio::select! {
                biased;
                interruption = control.interrupted() => {
                    drop(dispatch_start.take());
                    Err(interruption)
                },
                result = std::future::poll_fn(|cx| {
                    dispatch_polled = true;
                    crate::agent::cost::mark_goal_dispatch_entered();
                    let result = provider_future.as_mut().poll(cx);
                    drop(dispatch_start.take());
                    result
                }) => Ok(result),
            }
        };
        let result = match outcome {
            Ok(result) => result,
            Err(interruption) => {
                let accounting = scope.take();
                // Goal operations must settle known attempts before fencing a
                // successor. Ordinary turns retain their prior drop behavior.
                if dispatch_polled && crate::agent::cost::goal_operation_accounting_is_scoped() {
                    let attempts = accounting.attempts();
                    extend_rejected_summaries(settled_out, attempts);
                    crate::agent::cost::settle_interrupted_provider_attempts(attempts)
                        .await
                        .map_err(|error| {
                            crate::agent::cost::with_settlement_failure(interruption.into(), error)
                        })?;
                } else if !dispatch_polled {
                    release_unstarted_goal_operation_if_scoped()
                        .await
                        .map_err(|error| {
                            crate::agent::cost::with_visible_context(
                                error,
                                &format!("while handling {interruption}"),
                            )
                        })?;
                }
                return Err(interruption.into());
            }
        };
        if result.is_ok() {
            scope.mark_logical_success();
        }
        let accounting = scope.take();

        let attempts = accounting.attempts();
        crate::agent::cost::register_goal_returned_attempts(attempts);
        let goal_scoped = crate::agent::cost::goal_operation_accounting_is_scoped();

        let accepted_route = accounting.accepted_route().cloned();
        let accepted_attempt = accepted_route.as_ref().and_then(|_| attempts.last());
        let (served_provider, served_model) = accepted_route
            .as_ref()
            .map(|route| (route.provider_ref().to_string(), route.model().to_string()))
            .unwrap_or_else(|| (self.provider_name.to_string(), self.model.to_string()));
        let (accounted_provider, accounted_model) = accepted_attempt
            .map(|attempt| (attempt.provider_ref(), attempt.model()))
            .unwrap_or((&served_provider, &served_model));
        match result {
            Ok(response) => {
                // A terminal response without final text or tools was billed
                // but cannot be accepted. Keep it out of context-window fill
                // and successful response telemetry before returning its typed
                // cause to every one-shot caller.
                if response.is_semantically_empty_terminal() {
                    extend_rejected_summaries(settled_out, attempts);
                    let error = anyhow::Error::new(SemanticEmptyTerminalCompletion);
                    return Err(
                        match crate::agent::cost::settle_provider_attempts(attempts, None).await {
                            Ok(()) => error,
                            Err(settlement) => {
                                crate::agent::cost::with_settlement_failure(error, settlement)
                            }
                        },
                    );
                }
                zeroclaw_providers::dispatch::commit_accepted_provider_route(accepted_route);
                let settlement = if goal_scoped {
                    extend_rejected_summaries(
                        settled_out,
                        &attempts[..attempts.len().saturating_sub(1)],
                    );
                    crate::agent::cost::settle_provider_attempts(
                        attempts,
                        Some(attempts.len().saturating_sub(1)),
                    )
                    .await
                } else {
                    extend_rejected_summaries(
                        settled_out,
                        &attempts[..attempts.len().saturating_sub(1)],
                    );
                    crate::agent::cost::settle_provider_attempts(
                        &attempts[..attempts.len().saturating_sub(1)],
                        None,
                    )
                    .await
                };
                // Only a semantically valid result controls accepted context
                // usage and successful response telemetry.
                let accepted_cost_usd = if goal_scoped {
                    response.usage.as_ref().and_then(|usage| {
                        accepted_attempt.map_or_else(
                            || {
                                crate::agent::cost::compute_cost_usd(
                                    &served_provider,
                                    &served_model,
                                    usage,
                                )
                            },
                            |attempt| crate::agent::cost::compute_attempt_cost_usd(attempt, usage),
                        )
                    })
                } else {
                    response.usage.as_ref().and_then(|usage| {
                        crate::agent::cost::record_accepted_attempt_cost_usage(
                            accounted_provider,
                            accounted_model,
                            usage,
                            accepted_attempt,
                        )
                        .map(|(_, cost_usd)| cost_usd)
                    })
                };
                // The accepted leaf is always projected (even usage-less, so
                // terminal identity stays coherent); the caller emits it with
                // `accepted: true`.
                settled_out.push(SettledAttemptSummary {
                    provider_ref: accounted_provider.to_string(),
                    model: accounted_model.to_string(),
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
                settlement?;
                Ok(response)
            }
            Err(error) => {
                extend_rejected_summaries(settled_out, attempts);
                Err(
                    match crate::agent::cost::settle_provider_attempts(attempts, None).await {
                        Ok(()) => error,
                        Err(settlement) => {
                            crate::agent::cost::with_settlement_failure(error, settlement)
                        }
                    },
                )
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
    pub keep_tool_context_turns: usize,
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
    pub keep_tool_context_turns: usize,
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
            keep_tool_context_turns: runtime.keep_tool_context_turns,
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

    struct OneShotFenceProvider {
        store: Arc<crate::control_plane::SqliteTaskStore>,
        scope: zeroclaw_goal::GoalExecutionScope,
        calls: Arc<AtomicUsize>,
    }

    impl Attributable for OneShotFenceProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "one-shot-fence-provider"
        }
    }

    #[async_trait]
    impl ModelProvider for OneShotFenceProvider {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            unreachable!("the fixture uses structured chat")
        }
        async fn chat(
            &self,
            _: ChatRequest<'_>,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            use zeroclaw_goal::GoalTaskRegistry;
            self.calls.fetch_add(1, Ordering::SeqCst);
            let task = self
                .store
                .current_goal_for_session(self.scope.session_id())
                .await?
                .unwrap();
            assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Running);
            tokio::task::yield_now().await;
            Ok(ChatResponse {
                text: Some("done".into()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(2),
                    output_tokens: Some(1),
                    cached_input_tokens: Some(0),
                    cache_creation_input_tokens: Some(0),
                }),
                reasoning_content: None,
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_shot_goal_dispatch_orders_entry_against_fence() {
        use crate::goal_accounting::tests::{DispatchProbe, DispatchProbeMode};
        use zeroclaw_goal::accounting::GoalOperationAccountingFactory;
        use zeroclaw_goal::{GoalAccountingState, GoalTaskRegistry, GoalTransitionResult};

        for before in [true, false] {
            let (store, factory, _tracker, scope, _directory) =
                crate::goal_accounting::tests::fixture().await;
            let probe = Arc::new(DispatchProbe::new(
                factory.for_scope(scope.clone()),
                Arc::clone(&store),
                scope.clone(),
                if before {
                    DispatchProbeMode::FenceBeforeStart
                } else {
                    DispatchProbeMode::QueueFenceAfterStart
                },
            ));
            let calls = Arc::new(AtomicUsize::new(0));
            let provider = OneShotFenceProvider {
                store: Arc::clone(&store),
                scope: scope.clone(),
                calls: Arc::clone(&calls),
            };
            let messages = [ChatMessage::user("hi")];
            let mut summaries = Vec::new();
            let result = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                .scope(Some(probe.clone()), async {
                    ResolvedModelAccess {
                        model_provider: &provider,
                        provider_name: "fallback",
                        model: "model",
                        dispatch_model: "model",
                        temperature: None,
                    }
                    .run_model_query_controlled(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        &mut summaries,
                        super::ModelQueryControl {
                            deadline: None,
                            cancellation_token: None,
                            foreground_token: None,
                        },
                    )
                    .await
                })
                .await;
            if before {
                assert!(
                    result
                        .unwrap_err()
                        .downcast_ref::<zeroclaw_goal::accounting::GoalOperationFenced>()
                        .is_some()
                );
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(probe.releases.load(Ordering::SeqCst), 1);
                assert_eq!(probe.settlements.load(Ordering::SeqCst), 0);
            } else {
                assert!(result.is_ok(), "{result:?}");
                assert_eq!(
                    probe.wait_for_fence().await.unwrap(),
                    GoalTransitionResult::Applied
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert_eq!(probe.releases.load(Ordering::SeqCst), 0);
                assert_eq!(probe.settlements.load(Ordering::SeqCst), 1);
                assert_eq!(summaries.len(), 1);
            }
            let task = store
                .current_goal_for_session(scope.session_id())
                .await
                .unwrap()
                .unwrap();
            let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
            assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert!(goal.pending_call_id.is_none());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_shot_failed_pause_keeps_goal_running_after_dispatch() {
        use crate::goal_accounting::tests::{DispatchProbe, DispatchProbeMode};
        use zeroclaw_goal::accounting::{GoalOperationAccountingFactory, GoalOperationRequest};
        use zeroclaw_goal::{GoalAccountingState, GoalTaskRegistry};

        let directory = tempfile::TempDir::new().unwrap();
        let store = Arc::new(crate::control_plane::SqliteTaskStore::new(directory.path()).unwrap());
        let (store, factory, _tracker, scope, directory) =
            crate::goal_accounting::tests::fixture_with_store(store, directory).await;
        let connection =
            rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_fixture_pause BEFORE UPDATE OF status ON tasks
             WHEN NEW.status = 'paused'
             BEGIN SELECT RAISE(ABORT, 'fixture pause write failed'); END;",
            )
            .unwrap();
        drop(connection);
        let probe = Arc::new(DispatchProbe::new(
            factory.for_scope(scope.clone()),
            Arc::clone(&store),
            scope.clone(),
            DispatchProbeMode::QueueFenceAfterStart,
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = OneShotFenceProvider {
            store: Arc::clone(&store),
            scope: scope.clone(),
            calls: Arc::clone(&calls),
        };
        let messages = [ChatMessage::user("hi")];
        let mut summaries = Vec::new();
        let result = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(Some(probe.clone()), async {
                ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "fallback",
                    model: "model",
                    dispatch_model: "model",
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
            .await;
        assert!(result.is_ok(), "{result:?}");
        assert!(
            format!("{:#}", probe.wait_for_fence().await.unwrap_err())
                .contains("fixture pause write failed")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let task = store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Running);
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        let next = factory.for_scope(scope);
        next.admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        next.release_unstarted().await.unwrap();
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
        fn admission_guard(&self) -> anyhow::Result<zeroclaw_goal::accounting::GoalOperationGuard> {
            anyhow::bail!("refused query unexpectedly acquired an admission guard")
        }

        async fn begin_dispatch(&self) -> anyhow::Result<zeroclaw_goal::task::GoalDispatchStart> {
            anyhow::bail!("refused query unexpectedly reached the dispatch gate")
        }

        async fn release_unstarted(&self) -> anyhow::Result<()> {
            anyhow::bail!("refused query unexpectedly released admission")
        }

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

    struct FailingGoalSettlement;

    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for FailingGoalSettlement {
        fn admission_guard(&self) -> anyhow::Result<zeroclaw_goal::accounting::GoalOperationGuard> {
            Ok(zeroclaw_goal::accounting::GoalOperationGuard::new(()))
        }
        async fn begin_dispatch(&self) -> anyhow::Result<zeroclaw_goal::task::GoalDispatchStart> {
            Ok(zeroclaw_goal::task::GoalDispatchStart::new(
                std::sync::Arc::new(tokio::sync::Mutex::new(()))
                    .lock_owned()
                    .await,
            ))
        }
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
            anyhow::bail!("fixture settlement failed")
        }
        async fn release_unstarted(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn pricing_complete(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn run_model_query_preserves_provider_error_and_usage_when_settlement_fails() {
        let reliable = ReliableModelProvider::new(
            "reliable",
            vec![(
                "primary".to_owned(),
                Box::new(SemanticEmptyThenTextProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    persist_empty: true,
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let accounting: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(FailingGoalSettlement);
        let messages = [ChatMessage::user("hi")];
        let mut summaries = Vec::new();
        let error = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(Some(accounting), async {
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
            .expect_err("provider and settlement failures must be reported");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableRejectedCompletionUsage>())
        );
        assert!(format!("{error:#}").contains("fixture settlement failed"));
        let provider_error = error
            .chain()
            .find(|cause| cause.is::<ReliableRejectedCompletionUsage>())
            .unwrap()
            .to_string();
        assert!(error.to_string().contains(&provider_error));
        assert!(error.to_string().contains("fixture settlement failed"));
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].input_tokens, Some(80));
        assert_eq!(summaries[0].output_tokens, Some(5));
        assert!(!summaries[0].accepted);
    }

    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for CapturedGoalSettlement {
        fn admission_guard(&self) -> anyhow::Result<zeroclaw_goal::accounting::GoalOperationGuard> {
            Ok(zeroclaw_goal::accounting::GoalOperationGuard::new(()))
        }
        async fn begin_dispatch(&self) -> anyhow::Result<zeroclaw_goal::task::GoalDispatchStart> {
            Ok(zeroclaw_goal::task::GoalDispatchStart::new(
                std::sync::Arc::new(tokio::sync::Mutex::new(()))
                    .lock_owned()
                    .await,
            ))
        }
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

        async fn release_unstarted(&self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn pricing_complete(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    struct TypedFailingGoalSettlement;
    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for TypedFailingGoalSettlement {
        fn admission_guard(&self) -> anyhow::Result<zeroclaw_goal::accounting::GoalOperationGuard> {
            Ok(zeroclaw_goal::accounting::GoalOperationGuard::new(()))
        }
        async fn begin_dispatch(&self) -> anyhow::Result<zeroclaw_goal::task::GoalDispatchStart> {
            Ok(zeroclaw_goal::task::GoalDispatchStart::new(
                Arc::new(tokio::sync::Mutex::new(())).lock_owned().await,
            ))
        }
        async fn release_unstarted(&self) -> anyhow::Result<()> {
            Ok(())
        }
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
            Arc::new(TypedFailingGoalSettlement);
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
    async fn settlement_failure_preserves_accepted_usage_of_valid_response() {
        let provider = UsageProvider;
        let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(TypedFailingGoalSettlement);
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
        assert!(summaries[0].accepted);
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
            Arc::new(TypedFailingGoalSettlement);
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
    async fn run_model_query_prices_anthropic_fallback_attempts_by_attribution() {
        if super::super::run_anthropic_test_in_fresh_process(
            "agent::turn::execution::run_model_query_tests::run_model_query_prices_anthropic_fallback_attempts_by_attribution",
            "ZEROCLAW_ANTHROPIC_FALLBACK_QUERY_TEST_CHILD",
        ) {
            return;
        }

        use std::collections::HashMap;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};
        use zeroclaw_providers::anthropic::AnthropicModelProvider;

        for attributed in [true, false] {
            let server = MockServer::start().await;
            let terminal = if attributed {
                serde_json::json!({"type": "fallback_message", "model": "model-b", "input_tokens": 50, "output_tokens": 5})
            } else {
                serde_json::json!({"type": "fallback_message"})
            };
            let mut body = serde_json::json!({
                "content": [{"type": "text", "text": "done"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 50, "output_tokens": 5, "iterations": [
                    {"type": "message", "model": "model-a", "input_tokens": 100, "output_tokens": 10},
                    terminal
                ]}
            });
            if attributed {
                body["model"] = serde_json::json!("model-b");
            }
            Mock::given(method("POST"))
                .and(path("/v1/messages"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            let provider = AnthropicModelProvider::builder("test")
                .credential(Some("test-key"))
                .base_url(&server.uri())
                .server_fallback_models(vec!["model-b".to_string()])
                .build();
            let workspace = tempfile::TempDir::new().unwrap();
            let tracker = Arc::new(
                crate::cost::CostTracker::new(
                    zeroclaw_config::schema::CostConfig::default(),
                    workspace.path(),
                )
                .unwrap(),
            );
            let ctx = ToolLoopCostTrackingContext::new(
                tracker,
                Arc::new(HashMap::from([(
                    "anthropic.test".to_string(),
                    HashMap::from([
                        ("model-a.input".to_string(), 1.0),
                        ("model-a.output".to_string(), 2.0),
                        ("model-b.input".to_string(), 3.0),
                        ("model-b.output".to_string(), 4.0),
                        ("(unattributed).input".to_string(), 99.0),
                        ("(unattributed).output".to_string(), 99.0),
                    ]),
                )])),
            );
            let turn_usage = Arc::clone(&ctx.turn_usage);
            let messages = [ChatMessage::user("hello")];
            let mut summaries = Vec::new();
            TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(ctx), async {
                    ResolvedModelAccess {
                        model_provider: &provider,
                        provider_name: "anthropic.test",
                        model: "model-a",
                        dispatch_model: "model-a",
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
                .expect("fallback response");
            assert_eq!(summaries.len(), 2);
            assert_eq!(summaries[0].model, "model-a");
            assert!(!summaries[0].accepted);
            assert!((summaries[0].cost_usd.unwrap() - 120.0 / 1_000_000.0).abs() < 1e-12);
            assert_eq!(
                summaries[1].model,
                if attributed {
                    "model-b"
                } else {
                    "(unattributed)"
                }
            );
            assert!(summaries[1].accepted);
            let expected = if attributed { 170.0 / 1_000_000.0 } else { 0.0 };
            assert!((summaries[1].cost_usd.unwrap() - expected).abs() < 1e-12);
            let usage = *turn_usage.lock();
            assert_eq!(
                (
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.last_input_tokens
                ),
                (150, 15, 50)
            );
            assert!((usage.cost_usd - (120.0 / 1_000_000.0 + expected)).abs() < 1e-12);
            let ledger =
                std::fs::read_to_string(workspace.path().join("state").join("costs.jsonl"))
                    .expect("persisted cost ledger");
            let rows: Vec<zeroclaw_config::cost::types::CostRecord> = ledger
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].usage.model, "model-a");
            assert_eq!(rows[0].usage.total_tokens, 110);
            assert_eq!(
                rows[1].usage.model,
                if attributed {
                    "model-b"
                } else {
                    "(unattributed)"
                }
            );
            assert_eq!(rows[1].usage.total_tokens, 55);
            assert_eq!(rows[1].usage.pricing_available, attributed);
            assert_eq!(
                rows[1].usage.unpriced_tokens,
                if attributed { 0 } else { 55 }
            );
        }
    }

    #[tokio::test]
    async fn goal_anthropic_fallback_settles_per_model_and_fences_uncertainty() {
        if super::super::run_anthropic_test_in_fresh_process(
            "agent::turn::execution::run_model_query_tests::goal_anthropic_fallback_settles_per_model_and_fences_uncertainty",
            "ZEROCLAW_GOAL_ANTHROPIC_SETTLEMENT_TEST_CHILD",
        ) {
            return;
        }

        use std::collections::HashMap;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};
        use zeroclaw_goal::accounting::{GoalOperationAccountingFactory, GoalOperationRequest};
        use zeroclaw_goal::{GoalAccountingState, GoalTaskRegistry, GoalTransitionResult};
        use zeroclaw_providers::anthropic::AnthropicModelProvider;

        // A top-level model without a matching iteration model is priced as B
        // while remaining unsettled; complete but unpriced usage also fences
        // the next operation when the Goal has a cost limit.
        for (
            terminal_model,
            top_model,
            expected_state,
            refusal,
            invalid_first,
            unpriced_terminal,
        ) in [
            (
                Some("model-b"),
                Some("model-b"),
                GoalAccountingState::Complete,
                false,
                false,
                false,
            ),
            (
                None,
                None,
                GoalAccountingState::OutcomeUnknown,
                false,
                false,
                false,
            ),
            (
                None,
                Some("model-b"),
                GoalAccountingState::OutcomeUnknown,
                false,
                false,
                false,
            ),
            (
                Some("model-b"),
                Some("model-b"),
                GoalAccountingState::Complete,
                true,
                false,
                false,
            ),
            (
                None,
                None,
                GoalAccountingState::OutcomeUnknown,
                true,
                false,
                false,
            ),
            (
                Some("model-b"),
                Some("model-b"),
                GoalAccountingState::Invalid,
                false,
                true,
                false,
            ),
            (
                Some("model-b"),
                Some("model-b"),
                GoalAccountingState::Complete,
                false,
                false,
                true,
            ),
        ] {
            let server = MockServer::start().await;
            let mut terminal = serde_json::json!({"type": "fallback_message"});
            if let Some(model) = terminal_model {
                terminal["model"] = serde_json::json!(model);
                terminal["input_tokens"] = serde_json::json!(50);
                terminal["output_tokens"] = serde_json::json!(5);
            }
            let mut first =
                serde_json::json!({"type": "message", "model": "model-a", "input_tokens": 100});
            if !invalid_first {
                first["output_tokens"] = serde_json::json!(10);
            }
            let mut body = serde_json::json!({
                "content": [{"type": "text", "text": "done"}],
                "stop_reason": if refusal { "refusal" } else { "end_turn" },
                "usage": {"input_tokens": 50, "output_tokens": 5, "iterations": [
                    first,
                    terminal
                ]}
            });
            if let Some(model) = top_model {
                body["model"] = serde_json::json!(model);
            }
            Mock::given(method("POST"))
                .and(path("/v1/messages"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            let provider = AnthropicModelProvider::builder("test")
                .credential(Some("test-key"))
                .base_url(&server.uri())
                .server_fallback_models(vec!["model-b".to_string()])
                .build();
            let (store, _fixture_factory, tracker, scope, directory) =
                crate::goal_accounting::tests::fixture().await;
            let mut rates = HashMap::from([
                ("model-a.input".to_string(), 1.0),
                ("model-a.output".to_string(), 2.0),
                ("model-b.input".to_string(), 3.0),
                ("model-b.output".to_string(), 4.0),
                ("(unattributed).input".to_string(), 99.0),
                ("(unattributed).output".to_string(), 99.0),
            ]);
            if unpriced_terminal {
                rates.remove("model-b.input");
                rates.remove("model-b.output");
                assert_eq!(
                    store
                        .update_session_goal_limits(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            None,
                            Some(10.0),
                        )
                        .await
                        .unwrap(),
                    GoalTransitionResult::Applied
                );
            }
            let pricing = Arc::new(HashMap::from([("anthropic.test".to_string(), rates)]));
            let factory = crate::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
                store.clone() as Arc<dyn GoalTaskRegistry>,
                Arc::clone(&tracker),
                "main",
                Arc::new(crate::goal_accounting::ModelProviderGoalUsagePricer::new(
                    Arc::clone(&pricing),
                )),
            )
            .unwrap();
            let accountant = factory.for_scope(scope.clone());
            let context = ToolLoopCostTrackingContext::new(tracker, pricing);
            let turn_usage = Arc::clone(&context.turn_usage);
            let messages = [ChatMessage::user("hello")];
            let mut summaries = Vec::new();
            let result = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                .scope(Some(accountant), async {
                    TOOL_LOOP_COST_TRACKING_CONTEXT
                        .scope(Some(context), async {
                            ResolvedModelAccess {
                                model_provider: &provider,
                                provider_name: "anthropic.test",
                                model: "model-a",
                                dispatch_model: "model-a",
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
                })
                .await;
            if expected_state != GoalAccountingState::Complete {
                let error = result.unwrap_err();
                let marker = error
                    .downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>()
                    .unwrap();
                assert_eq!(marker.accounting_state(), expected_state);
                assert!(!marker.has_pending_operation());
                assert_eq!(
                    error.is::<zeroclaw_api::model_provider::ModelRefusalError>(),
                    refusal
                );
            } else if refusal {
                assert!(
                    result
                        .unwrap_err()
                        .downcast_ref::<zeroclaw_api::model_provider::ModelRefusalError>()
                        .is_some()
                );
            } else {
                assert!(result.is_ok(), "{result:?}");
            }

            let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
            assert_eq!(goal.accounting_state, expected_state);
            assert!(goal.pending_call_id.is_none());
            let ledger = std::fs::read_to_string(directory.path().join("state/costs.jsonl"))
                .expect("Goal ledger");
            let rows: Vec<zeroclaw_config::cost::types::CostRecord> = ledger
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 2, "each attempt must be recorded exactly once");
            assert!(
                rows.iter()
                    .all(|row| row.task_id.as_deref() == Some(scope.task_id()))
            );
            assert!(
                rows.iter()
                    .all(|row| row.provider_ref.as_deref() == Some("anthropic.test"))
            );
            assert_eq!(
                (rows[0].usage.model.as_str(), rows[0].usage.total_tokens),
                ("model-a", if invalid_first { 100 } else { 110 })
            );
            let terminal_attributed = top_model.is_some();
            assert_eq!(
                (rows[1].usage.model.as_str(), rows[1].usage.total_tokens),
                (
                    if terminal_attributed {
                        "model-b"
                    } else {
                        "(unattributed)"
                    },
                    55
                )
            );
            let terminal_priced = terminal_attributed && !unpriced_terminal;
            assert_eq!(rows[1].usage.pricing_available, terminal_priced);
            assert_eq!(
                rows[1].usage.unpriced_tokens,
                if terminal_priced { 0 } else { 55 }
            );
            let expected_terminal_cost = if terminal_priced { 170.0 / 1e6 } else { 0.0 };
            let first_ledger_cost = if invalid_first {
                100.0 / 1e6
            } else {
                120.0 / 1e6
            };
            assert!((rows[0].usage.cost_usd - first_ledger_cost).abs() < 1e-12);
            assert!((rows[1].usage.cost_usd - expected_terminal_cost).abs() < 1e-12);
            assert_eq!(summaries.len(), if invalid_first { 1 } else { 2 });
            let terminal_summary = summaries.last().unwrap();
            assert_eq!(terminal_summary.model, rows[1].usage.model);
            assert!((terminal_summary.cost_usd.unwrap() - expected_terminal_cost).abs() < 1e-12);
            assert_eq!(terminal_summary.accepted, !refusal);
            let usage = *turn_usage.lock();
            assert_eq!(
                (
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.last_input_tokens
                ),
                (
                    if invalid_first { 50 } else { 150 },
                    if invalid_first { 5 } else { 15 },
                    if refusal { 0 } else { 50 }
                )
            );
            let first_projected_cost = if invalid_first { 0.0 } else { 120.0 / 1e6 };
            assert!(
                (usage.cost_usd - (first_projected_cost + expected_terminal_cost)).abs() < 1e-12
            );

            let next = factory.for_scope(scope);
            let admission = next
                .admit(GoalOperationRequest::new("anthropic.test", "model-a"))
                .await;
            if expected_state == GoalAccountingState::Complete && !unpriced_terminal {
                admission.expect("complete accounting permits the next operation");
                next.release_unstarted().await.unwrap();
            } else {
                assert!(
                    admission.is_err(),
                    "incomplete accounting or pricing must fence the next operation"
                );
            }
        }
    }

    #[tokio::test]
    async fn goal_anthropic_handoff_without_iterations_retains_lower_bound_and_fences() {
        if super::super::run_anthropic_test_in_fresh_process(
            "agent::turn::execution::run_model_query_tests::goal_anthropic_handoff_without_iterations_retains_lower_bound_and_fences",
            "ZEROCLAW_GOAL_ANTHROPIC_HANDOFF_TEST_CHILD",
        ) {
            return;
        }

        use std::collections::HashMap;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};
        use zeroclaw_goal::accounting::{GoalOperationAccountingFactory, GoalOperationRequest};
        use zeroclaw_goal::{GoalAccountingState, GoalTaskRegistry};
        use zeroclaw_providers::anthropic::AnthropicModelProvider;

        // A handoff proves that an earlier attempt is missing even when the
        // returned attempt has usage. A response without a handoff is a control.
        for (handoff, refusal, has_model, foreign) in [
            (true, false, true, false),
            (true, true, true, false),
            (true, false, false, false),
            (false, false, true, false),
            (false, false, true, true),
        ] {
            let server = MockServer::start().await;
            let content = if handoff {
                serde_json::json!([{"type": "fallback"}, {"type": "text", "text": "done"}])
            } else {
                serde_json::json!([{"type": "text", "text": "done"}])
            };
            let mut body = serde_json::json!({
                "content": content,
                "stop_reason": if refusal { "refusal" } else { "end_turn" },
                "usage": {"input_tokens": 50, "output_tokens": 5}
            });
            if has_model {
                body["model"] = serde_json::json!("model-b");
            }
            if foreign {
                body["usage"]["iterations"] = serde_json::json!([
                    {"type": "message", "model": "model-a", "input_tokens": 100, "output_tokens": 10},
                    {"type": "compaction"}
                ]);
            }
            Mock::given(method("POST"))
                .and(path("/v1/messages"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            let provider = AnthropicModelProvider::builder("test")
                .credential(Some("test-key"))
                .base_url(&server.uri())
                .server_fallback_models(vec!["model-b".to_string()])
                .build();
            let (store, _fixture_factory, tracker, scope, directory) =
                crate::goal_accounting::tests::fixture().await;
            let pricing = Arc::new(HashMap::from([(
                "anthropic.test".to_string(),
                HashMap::from([
                    ("model-a.input".to_string(), 1.0),
                    ("model-a.output".to_string(), 2.0),
                    ("model-b.input".to_string(), 3.0),
                    ("model-b.output".to_string(), 4.0),
                    ("(unattributed).input".to_string(), 99.0),
                    ("(unattributed).output".to_string(), 99.0),
                ]),
            )]));
            let factory = crate::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
                store.clone() as Arc<dyn GoalTaskRegistry>,
                Arc::clone(&tracker),
                "main",
                Arc::new(crate::goal_accounting::ModelProviderGoalUsagePricer::new(
                    Arc::clone(&pricing),
                )),
            )
            .unwrap();
            let accountant = factory.for_scope(scope.clone());
            let context = ToolLoopCostTrackingContext::new(tracker, pricing);
            let messages = [ChatMessage::user("hello")];
            let mut summaries = Vec::new();
            let result = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                .scope(Some(accountant), async {
                    TOOL_LOOP_COST_TRACKING_CONTEXT
                        .scope(Some(context), async {
                            ResolvedModelAccess {
                                model_provider: &provider,
                                provider_name: "anthropic.test",
                                model: "model-a",
                                dispatch_model: "model-a",
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
                })
                .await;
            if handoff || foreign {
                let error = result.unwrap_err();
                let marker = error
                    .downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>()
                    .unwrap();
                assert_eq!(
                    marker.accounting_state(),
                    GoalAccountingState::OutcomeUnknown
                );
                assert!(!marker.has_pending_operation());
                assert_eq!(
                    error.is::<zeroclaw_api::model_provider::ModelRefusalError>(),
                    refusal
                );
            } else if refusal {
                assert!(
                    result
                        .unwrap_err()
                        .downcast_ref::<zeroclaw_api::model_provider::ModelRefusalError>()
                        .is_some()
                );
            } else {
                assert!(result.is_ok(), "{result:?}");
            }
            let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
            assert_eq!(
                goal.accounting_state,
                if handoff || foreign {
                    GoalAccountingState::OutcomeUnknown
                } else {
                    GoalAccountingState::Complete
                }
            );
            assert!(goal.pending_call_id.is_none());
            let ledger =
                std::fs::read_to_string(directory.path().join("state/costs.jsonl")).unwrap();
            let rows: Vec<zeroclaw_config::cost::types::CostRecord> = ledger
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 1, "the unobserved attempt must not be invented");
            assert_eq!(rows[0].task_id.as_deref(), Some(scope.task_id()));
            assert_eq!(rows[0].usage.total_tokens, 55);
            let expected_model = if foreign {
                "(unattributed)"
            } else if !handoff {
                "model-a"
            } else if has_model {
                "model-b"
            } else {
                "(unattributed)"
            };
            assert_eq!(rows[0].usage.model, expected_model);
            let attributed = !foreign && (!handoff || has_model);
            assert_eq!(rows[0].usage.pricing_available, attributed);
            assert_eq!(
                rows[0].usage.unpriced_tokens,
                if attributed { 0 } else { 55 }
            );
            let expected_cost = if foreign {
                0.0
            } else if !handoff {
                60.0 / 1e6
            } else if has_model {
                170.0 / 1e6
            } else {
                0.0
            };
            assert!((rows[0].usage.cost_usd - expected_cost).abs() < 1e-12);
            let next = factory.for_scope(scope);
            let admission = next
                .admit(GoalOperationRequest::new("anthropic.test", "model-a"))
                .await;
            if handoff || foreign {
                assert!(
                    admission.is_err(),
                    "unsettled handoff must fence the next operation"
                );
            } else {
                admission.expect("single-attempt control must continue");
                next.release_unstarted().await.unwrap();
            }
        }
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
