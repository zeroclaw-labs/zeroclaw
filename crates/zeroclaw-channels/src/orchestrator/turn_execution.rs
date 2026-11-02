//! Shared configured execution construction for channel foreground turns.
//!
//! This module deliberately owns no presentation, history persistence, or
//! transport work.  It keeps the ordinary channel tool-loop configuration in
//! one place so a future Goal session lease can use the same configured
//! provider, tool policy, approvals, and pacing without creating a second
//! channel execution host.

use zeroclaw_providers::ModelProvider;
use zeroclaw_runtime::{
    agent::loop_::{
        LoopKnobs, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess, ResolvedRuntimeKnobs,
    },
    approval::ApprovalManager,
    observability::Observer,
};

use super::{ChannelRouteSelection, ChannelRuntimeContext, resolve_channel_context_limits};

pub(super) fn resolved_channel_execution<'a>(
    context: &'a ChannelRuntimeContext,
    model_provider: &'a dyn ModelProvider,
    route: &'a ChannelRouteSelection,
    observer: &'a dyn Observer,
    approval_manager: &'a ApprovalManager,
    loop_knobs: &'a LoopKnobs,
    excluded_tools: &'a [String],
    temperature: Option<f64>,
) -> ResolvedAgentExecution<'a> {
    ResolvedAgentExecution::resolve(
        ResolvedModelAccess {
            model_provider,
            provider_name: route.model_provider.as_str(),
            model: route.model.as_str(),
            dispatch_model: route.model.as_str(),
            temperature,
        },
        ResolvedIo {
            tools_registry: context.tools_registry.as_ref(),
            observer,
            silent: true,
            approval: Some(approval_manager),
            // Preserve the same agent policy used by ordinary channel turns;
            // it governs whether image markers resolve before the no-vision
            // capability check is raised.
            security: Some(context.security.as_ref()),
            multimodal_config: &context.multimodal,
            config: Some(context.prompt_config.as_ref()),
            hooks: context.hooks.as_deref(),
            activated_tools: context.activated_tools.as_ref(),
            model_switch_callback: None,
            receipt_generator: context.receipt_generator.as_ref(),
        },
        ResolvedRuntimeKnobs {
            max_tool_iterations: context.max_tool_iterations,
            excluded_tools,
            dedup_exempt_tools: context.tool_call_dedup_exempt.as_ref(),
            pacing: &context.pacing,
            strict_tool_parsing: context.agent_cfg.resolved.strict_tool_parsing,
            parallel_tools: context.agent_cfg.resolved.parallel_tools,
            max_tool_result_chars: context.max_tool_result_chars,
            context_limits: resolve_channel_context_limits(
                context.prompt_config.as_ref(),
                context.agent_alias.as_str(),
                route,
                context.context_token_budget,
            ),
            context_limits_resolver: None,
            knobs: loop_knobs,
        },
    )
}
