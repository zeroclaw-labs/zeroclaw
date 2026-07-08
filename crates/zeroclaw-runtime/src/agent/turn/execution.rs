//! The resolved per-agent execution context the turn engine requires.
//!
//! `ToolLoop` (the engine's input) carries two kinds of state: values that are
//! stable for every turn to a given agent (the model binding, the gated tool
//! registry, the approval policy, the resolved runtime knobs) and values that
//! change every message (history, streaming sinks, steering, the ingress
//! envelope). This module groups the *stable* half into one bundle so the
//! engine accepts it as a single required input.
//!
//! Two layers:
//! - [`ResolvedModelAccess`]: the bare model binding (provider + model +
//!   temperature). Any LLM call needs it; the agent bundle composes it.
//! - [`ResolvedAgentExecution`]: the full per-agent policy: the model access
//!   plus the tool registry, approval, observability, and the resolved runtime
//!   knobs.
//!
//! G0 is a behavior-neutral regrouping: the field names mirror the engine's
//! former flat `ToolLoop` fields one-for-one, so the loop body is unchanged
//! after it destructures the bundle. Later epics move the *resolution* of these
//! fields into a single `resolve()` constructor and seal the inputs so a turn
//! cannot run with a partially- or un-resolved policy.

use std::sync::{Arc, Mutex};

use zeroclaw_api::model_provider::{ChatRequest, ChatResponse};
use zeroclaw_config::schema::{MultimodalConfig, PacingConfig};
use zeroclaw_providers::{ModelProvider, ProviderDispatch};

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
    /// The one metered, attribution-preserving, non-streaming provider call.
    ///
    /// Budget-gate, then dispatch through [`ProviderDispatch`] (which opens the
    /// single `attribution_span!` for the call - see #7748; do NOT wrap another),
    /// then record token usage against the enclosing tool-loop cost tracker.
    ///
    /// Metering degrades to a no-op (not an error) when there is no
    /// `TOOL_LOOP_COST_TRACKING_CONTEXT` scope on the current task (tests, CLI
    /// without cost tracking): the budget check allows and usage recording is
    /// skipped. This is the shared entry point for one-shot LLM queries that are
    /// not full agentic turns (skill reflection today; the reply-intent
    /// classifier, graceful-summary, and other ad-hoc callers as they are routed
    /// through it). The engine's streaming turn call stays in
    /// `provider_call::call_provider`, which additionally emits the
    /// `TurnCtx`-bound announce/observer surface this method deliberately omits.
    pub async fn run_model_query(&self, request: ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        // Fail closed before spending a provider call when the enclosing turn's
        // cost budget is already exhausted. No-op when unscoped.
        crate::agent::turn::provider_call::enforce_tool_loop_budget()?;
        let resp = ProviderDispatch::from_ref(self.model_provider)
            .chat(request, self.model, self.temperature)
            .await?;
        // Record spend immediately after the call (before any caller-side output
        // validation) so a downstream failure still counts the provider usage.
        if let Some(usage) = resp.usage.as_ref() {
            crate::agent::cost::record_tool_loop_cost_usage(self.provider_name, self.model, usage);
        }
        Ok(resp)
    }
}

/// The per-agent-stable execution context the turn engine requires: the model
/// binding plus the tool registry, policy, observability, and resolved runtime
/// knobs that do not change between messages to the same agent. The engine
/// takes this as one input; per-message state (history, streaming, steering,
/// ingress, cancellation) stays on `ToolLoop` alongside it.
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
    /// The single seam every turn-construction path produces the bundle through,
    /// so a turn's per-agent policy is assembled in one place rather than re-derived
    /// inline at each call site. Today it spreads already-resolved inputs into the
    /// bundle (behavior-neutral); later surface PRs move the per-field resolution
    /// (tools via a scoped registry, approval, the runtime knobs) into this
    /// constructor and seal the inputs, at which point the flat fields collapse into
    /// the [`ResolvedIo`] / [`ResolvedRuntimeKnobs`] layers passed here.
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
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{ChatRequest, ChatResponse};
    use zeroclaw_providers::traits::TokenUsage;
    use zeroclaw_providers::{ChatMessage, ModelProvider};

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
}
