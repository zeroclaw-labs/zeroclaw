//! Capabilities built from config by today's `create_*` factories.
//!
//! These sources reproduce the construction the entry points performed inline
//! before they took capabilities, so an adapter that routes through them
//! builds the same provider, memory and observer it did before. They are
//! public through [`super::defaults`] so the application layer composes the
//! same recipe instead of copying it.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::channel::Channel;
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::schema::Config;

use super::{
    ChannelSource, MemoryRequest, MemorySource, NativeRegistryRecipe, ProviderRequest,
    ProviderSource, RuntimeCapabilities, ToolRequest, ToolSource,
};

pub(super) fn capabilities_with_observer(
    observer: Arc<dyn zeroclaw_api::observability_traits::Observer>,
) -> RuntimeCapabilities {
    RuntimeCapabilities {
        providers: Arc::new(ConfigProviders),
        memory: Arc::new(ConfigMemory),
        tools: Arc::new(NoSuppliedTools),
        channels: Arc::new(NoOutboundChannels),
        observer,
    }
}

pub(super) fn capabilities(config: &Config) -> RuntimeCapabilities {
    capabilities_with_observer(Arc::from(crate::observability::create_observer(
        &config.observability,
    )))
}

pub(super) fn unobserved_capabilities() -> RuntimeCapabilities {
    capabilities_with_observer(Arc::new(crate::observability::NoopObserver))
}

/// Builds the routed, resilient provider the turn loop and the agent built,
/// and the agent's config-snapshot provider on a model switch.
pub struct ConfigProviders;

impl ProviderSource for ConfigProviders {
    fn model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        let config = request.config;
        let agent_entry = config.resolved_model_provider_for_agent(request.agent_alias);
        let provider_ref = match request.provider_ref {
            Some(provider_ref) => provider_ref.to_string(),
            None => agent_entry
                .map(|(family, alias, _)| format!("{family}.{alias}"))
                .ok_or_else(|| {
                    anyhow::Error::msg(format!(
                        "agents.{}.model_provider does not resolve to a configured \
                         [providers.models.<type>.<alias>] entry",
                        request.agent_alias
                    ))
                })?,
        };
        let target_entry = provider_ref
            .split_once('.')
            .and_then(|(family, alias)| config.providers.models.find(family, alias));
        let model = request
            .model
            .map(str::to_string)
            .or_else(|| {
                target_entry
                    .and_then(|entry| entry.model.as_deref())
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "model_provider `{provider_ref}` has no `model` configured and no model \
                     was requested"
                ))
            })?;

        // A configured target entry is the canonical source of credentials and
        // endpoint. The agent's own entry is the fallback only when the
        // reference names no configured entry, as a bare-family override does.
        let credential_entry = target_entry.or(agent_entry.map(|(_, _, entry)| entry));
        let api_key = credential_entry.and_then(|entry| entry.api_key.as_deref());
        let uri = credential_entry.and_then(|entry| entry.uri.as_deref());

        let agent_options = match agent_entry {
            Some((family, alias, _)) => {
                zeroclaw_providers::provider_runtime_options_for_alias(config, family, alias)
            }
            None => {
                zeroclaw_providers::provider_runtime_options_for_agent(config, request.agent_alias)
            }
        };
        let options =
            zeroclaw_providers::options_for_provider_ref(config, &provider_ref, &agent_options);

        let provider = zeroclaw_providers::create_routed_model_provider_with_options(
            config,
            &provider_ref,
            api_key,
            uri,
            &config.reliability,
            &config.model_routes,
            &model,
            &options,
        )?;
        Ok(Arc::from(provider))
    }

    /// The agent's config-snapshot switch recipe, so a capability-built agent
    /// on these sources switches exactly as an adapter-built agent does. It
    /// differs from a session start in two places: a matching `model_routes`
    /// credential is preferred, and a bare-family target takes only the
    /// root multimodal policy rather than the agent's other options.
    fn switched_model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        let (Some(provider_ref), Some(model)) = (request.provider_ref, request.model) else {
            return self.model_provider(request);
        };
        let (provider, _resolver) = crate::agent::agent::config_switch_provider(
            request.config,
            request.agent_alias,
            provider_ref,
            model,
        )?;
        Ok(Arc::from(provider))
    }

    /// The vision route is built through the alias-aware factory the turn's
    /// vision routing has always used, so its per-alias `vision` override,
    /// endpoint and credentials are honored exactly as before.
    fn vision_model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        let Some(provider_ref) = request.provider_ref else {
            return self.model_provider(request);
        };
        let resolved = zeroclaw_providers::create_model_provider_from_ref_with_model(
            request.config,
            provider_ref,
        )?;
        Ok(Arc::from(resolved.provider))
    }
}

/// Opens the agent's memory with the agent provider's credential, which
/// embedding resolution inherits when `[memory]` names none of its own.
pub struct ConfigMemory;

#[async_trait]
impl MemorySource for ConfigMemory {
    async fn memory(&self, request: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>> {
        let api_key = request
            .config
            .resolved_model_provider_for_agent(request.agent_alias)
            .and_then(|(_, _, entry)| entry.api_key.as_deref());
        zeroclaw_memory::create_memory_for_agent(request.config, request.agent_alias, api_key).await
    }
}

/// Every tool is still built by the runtime's own registry, so the
/// config-backed set supplies none of its own.
pub struct NoSuppliedTools;

impl ToolSource for NoSuppliedTools {
    fn native_recipe<'s>(
        &'s self,
        _request: &ToolRequest<'_>,
    ) -> anyhow::Result<Option<&'s NativeRegistryRecipe>> {
        Ok(Some(&native_compat_registry))
    }
    fn tools(&self, _request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>> {
        Ok(Vec::new())
    }
}

/// The runtime's native recipe: builds the native registry from the request's
/// config with the caller's own handles. It reads configuration facts only;
/// the policy, runtime and memory come from the request unchanged.
pub fn native_compat_registry(
    inputs: super::NativeRecipeInputs<'_>,
) -> anyhow::Result<crate::tools::AllToolsResult> {
    let super::NativeRecipeInputs {
        request,
        workspace_dir,
        canvas_store,
        is_subagent_caller,
        forwarded_environment: tui_env,
        sop_engine,
        sop_audit,
        live_config,
        execution_capability,
        acp_sessions,
    } = inputs;
    let config = request.config.as_ref();
    let risk_profile = config
        .risk_profile_for_agent(request.agent_alias)
        .ok_or_else(|| anyhow::Error::msg("native registry risk profile is not configured"))?;
    let composio_key = config
        .composio
        .enabled
        .then_some(config.composio.api_key.as_deref())
        .flatten();
    let composio_entity = config
        .composio
        .enabled
        .then_some(config.composio.entity_id.as_str());
    let fallback_key = config
        .resolved_model_provider_for_agent(request.agent_alias)
        .and_then(|(_, _, provider)| provider.api_key.as_deref());
    crate::tools::all_tools_with_runtime_context(
        Arc::clone(request.config),
        request.security,
        risk_profile,
        request.agent_alias,
        Arc::clone(request.runtime),
        Arc::clone(request.memory),
        composio_key,
        composio_entity,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        workspace_dir,
        &config.agents,
        fallback_key,
        config,
        canvas_store,
        is_subagent_caller,
        tui_env,
        sop_engine,
        sop_audit,
        live_config,
        execution_capability,
        acp_sessions,
    )
}

/// The config-backed channel source. Despite its name it serves one channel:
/// the `cli` alias, from the CLI channel factory the application registered.
/// Every other alias is `None`; outbound channels are not routed through it.
pub struct NoOutboundChannels;

impl ChannelSource for NoOutboundChannels {
    fn channel(&self, alias: &str) -> Option<Arc<dyn Channel>> {
        (alias == "cli")
            .then(|| crate::agent::loop_::CLI_CHANNEL_FN.get())
            .flatten()
            .map(|factory| Arc::from(factory()))
    }
}
