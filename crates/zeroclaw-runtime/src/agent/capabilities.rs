//! Capability-taking construction for embedded session turns. No concrete
//! provider, memory, observer or eager tool factory runs on this path.
use super::agent::{Agent, tool_dispatcher_for_provider};
use crate::composition::{MemoryRequest, ProviderRequest, RuntimeCapabilities, ToolRequest};
use crate::tools::scoped::{ScopedAssembly, ScopedToolRegistry};
use anyhow::{Context, Result};
use std::sync::Arc;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::policy::SecurityPolicy;

impl Agent {
    /// Constructs the existing engine from supplied sources. Session admission
    /// and live authorization are still required before calling it. The caller
    /// supplies its currently resolved tool ceiling; an empty list admits none.
    pub async fn from_capabilities(
        live_config: &zeroclaw_config::live::LiveConfigHandle,
        agent_alias: &str,
        principal: &PrincipalId,
        caller_allowed_tools: &[String],
        capabilities: &RuntimeCapabilities,
        runtime: Arc<dyn RuntimeAdapter>,
    ) -> Result<Self> {
        // This creates the provider's construction generation; it is never
        // the policy authority. Live checks retain the supplied read handle.
        let generation = Arc::new(live_config.snapshot());
        let config = &generation;
        let agent_cfg = config
            .resolved_agent_config(agent_alias)
            .with_context(|| format!("agents.{agent_alias} is not configured"))?;
        anyhow::ensure!(agent_cfg.enabled, "agent is disabled");
        let risk_profile = config
            .risk_profile_for_agent(agent_alias)
            .context("agent risk profile missing")?;
        let security = Arc::new(SecurityPolicy::for_agent(config, agent_alias)?);
        let provider_ref = agent_cfg.model_provider.to_string();
        let provider = capabilities.providers.model_provider(&ProviderRequest {
            config,
            agent_alias,
            provider_ref: Some(&provider_ref),
            principal: Some(principal),
        })?;
        let memory = capabilities.memory.memory(&MemoryRequest {
            config,
            agent_alias,
        })?;
        let tools = capabilities.tools.tools(&ToolRequest {
            config,
            agent_alias,
            security: &security,
            runtime: &runtime,
            memory: &memory,
        })?;
        // Seal and filter supplied eager tools through the existing owner.
        // The source owns external construction; assembly cannot add hidden
        // peripheral or MCP capabilities to an embedder's supplied surface.
        let assembled = ScopedToolRegistry::assemble(ScopedAssembly {
            config,
            agent_alias,
            security: &security,
            built: crate::tools::AllToolsResult::from_prebuilt_tools(tools),
            skills: &[],
            runtime: Arc::clone(&runtime),
            caller_allowed: Some(caller_allowed_tools),
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            acp_delivery: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await;
        let route = config
            .resolved_model_provider_for_agent(agent_alias)
            .context("agent provider profile missing")?
            .2;
        let model = route.model.clone().context("agent model missing")?;
        let dispatcher = tool_dispatcher_for_provider(&agent_cfg, provider.as_ref(), &model);
        let limits_config = live_config.clone();
        let limits_alias = agent_alias.to_owned();
        let history_config = live_config.clone();
        let history_alias = agent_alias.to_owned();
        let mut agent = Self::builder()
            .model_provider(Box::new(provider))
            .memory(memory)
            .memory_security(Arc::clone(&security))
            .observer(Arc::clone(&capabilities.observer))
            .tools(assembled.registry)
            .allowed_tools(Some(caller_allowed_tools.to_vec()))
            .tool_dispatcher(dispatcher)
            .agent_alias(agent_alias.into())
            .model_provider_name(provider_ref)
            .model_name(model)
            .temperature(route.temperature)
            .config(agent_cfg)
            .workspace_dir(security.workspace_dir.clone())
            .agent_workspace_dir(config.agent_workspace_dir(agent_alias))
            .multimodal_config(config.multimodal.clone())
            .shell_profile(runtime.shell_profile())
            .security(Arc::clone(&security))
            .security_summary(Some(security.prompt_summary()))
            .autonomy_level(risk_profile.level)
            .approval_route(risk_profile.approval_route.clone())
            .approval_manager(Some(Arc::new(
                crate::approval::ApprovalManager::for_non_interactive_backchannel(risk_profile),
            )))
            .context_limits_resolver(Arc::new(move |provider_ref, model| {
                limits_config.read().resolved_context_limits_for_route(&limits_alias, provider_ref, model)
            }))
            .structured_history_turn_limit_resolver(Arc::new(move || {
                history_config.read().effective_structured_max_history_messages(&history_alias)
            }))
            .auto_save(config.memory.auto_save)
            // The model generation may remain pinned to its constructed
            // provider, but tool policy always resolves the live authority.
            .provider_switch_config(super::agent::ProviderSwitchConfig {
                config: Some(Arc::clone(config)),
                live_config: Some(live_config.clone()),
                live: None,
            })
            .build()?;
        // Resolve only the explicit approval route. ChannelSource is outbound,
        // never an implicit identity/admission source.
        if let Some(route) = &risk_profile.approval_route
            && let Some(channel) = capabilities.channels.channel(&route.approver_channel)
        {
            agent.populate_channels(&std::collections::HashMap::from([(
                route.approver_channel.clone(),
                channel,
            )]));
        }
        if *principal != PrincipalId::shared_operator() {
            agent.route_memory_to_principal(zeroclaw_api::memory_traits::PrincipalScope::new(
                principal.as_str(),
            ))?;
        }
        Ok(agent)
    }
}
