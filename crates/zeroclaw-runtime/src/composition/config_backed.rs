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
        let provider_ref = request
            .provider_ref
            .or_else(|| {
                request
                    .config
                    .agent(request.agent_alias)
                    .map(|agent| agent.model_provider.as_str())
            })
            .ok_or_else(|| {
                anyhow::Error::msg(format!("agents.{} is not configured", request.agent_alias))
            })?;
        zeroclaw_providers::model::build_model(
            request.config,
            request.agent_alias,
            provider_ref,
            request.model,
        )
        .map(|model| Arc::from(model.into_provider()))
    }

    fn switched_model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        self.model_provider(request)
    }

    fn vision_model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        let Some(provider_ref) = request.provider_ref else {
            return self.model_provider(request);
        };
        let resolved = zeroclaw_providers::model::build_raw_provider(
            request.config,
            provider_ref,
            zeroclaw_providers::model::ProviderOverrides {
                model: request.model,
                ..Default::default()
            },
        )?;
        Ok(Arc::from(resolved.provider))
    }
}

/// Opens the agent's memory. Embedding resolves its own connection credentials
/// independently of the agent's chat model.
pub struct ConfigMemory;

#[async_trait]
impl MemorySource for ConfigMemory {
    async fn memory(&self, request: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>> {
        zeroclaw_memory::create_memory_for_agent(request.config, request.agent_alias).await
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
    let risk_profile = inputs
        .request
        .config
        .risk_profile_for_agent(inputs.request.agent_alias)
        .ok_or_else(|| anyhow::Error::msg("native registry risk profile is not configured"))?;
    crate::tools::build_native_registry(inputs, risk_profile)
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
