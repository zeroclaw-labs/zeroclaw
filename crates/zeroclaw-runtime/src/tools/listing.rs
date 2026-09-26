//! An agent's tool listing: the policy-filtered tool specs a turn for that
//! agent would see, assembled the way a turn assembles them but never
//! invoked. The gateway's `/api/tools` registries and the `tools/list` RPC
//! method both come from here.

use std::sync::Arc;

use zeroclaw_config::schema::Config;

use super::{CanvasStore, ToolSpec, scoped};
use crate::platform::RuntimeAdapter;
use crate::security::SecurityPolicy;
use crate::sop::{SopAuditLogger, SopEngine};

/// What an agent's tool listing is assembled from, besides config.
#[derive(Clone)]
pub struct ToolListingDeps {
    pub runtime: Arc<dyn RuntimeAdapter>,
    pub memory: Arc<dyn zeroclaw_memory::Memory>,
    pub canvas_store: CanvasStore,
    pub sop_engine: Option<Arc<std::sync::Mutex<SopEngine>>>,
    pub sop_audit: Option<Arc<SopAuditLogger>>,
}

/// The smallest enabled agent alias, the one a listing without an agent
/// describes. Deterministic, so every caller picks the same agent.
#[must_use]
pub fn default_listing_alias(config: &Config) -> Option<String> {
    config
        .agents
        .iter()
        .filter(|(_, agent)| agent.enabled)
        .map(|(alias, _)| alias.clone())
        .min()
}

/// The tool specs agent `alias` would see, or `None` when its risk profile or
/// security policy does not resolve.
///
/// MCP servers are connected so their tools are listed, peripherals are not
/// (a listing must never hold hardware a live turn needs), and no tool runs.
pub async fn agent_tool_specs(
    config: &Config,
    alias: &str,
    deps: &ToolListingDeps,
) -> anyhow::Result<Option<Vec<ToolSpec>>> {
    let Some(risk_profile) = config.risk_profile_for_agent(alias) else {
        return Ok(None);
    };
    let risk_profile = risk_profile.clone();
    let Ok(security) = SecurityPolicy::for_agent(config, alias) else {
        return Ok(None);
    };
    let security = Arc::new(security);
    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let built = super::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        &risk_profile,
        alias,
        Arc::clone(&deps.runtime),
        Arc::clone(&deps.memory),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.data_dir,
        &config.agents,
        config
            .model_provider_for_agent(alias)
            .and_then(|entry| entry.api_key.as_deref()),
        config,
        Some(deps.canvas_store.clone()),
        false,
        None,
        deps.sop_engine.clone(),
        deps.sop_audit.clone(),
        None,
    )?;
    let assembled = scoped::ScopedToolRegistry::assemble(scoped::ScopedAssembly {
        config,
        agent_alias: alias,
        security: &security,
        built,
        skills: &[],
        runtime: Arc::clone(&deps.runtime),
        caller_allowed: None,
        connect_mcp: true,
        mcp_registry: None,
        connect_peripherals: false,
        emit_assembly_logs: false,
        exclude_memory: false,
        acp_delivery: false,
        list_deferred_mcp_specs: true,
    })
    .await;
    Ok(Some(
        assembled.registry.iter().map(|tool| tool.spec()).collect(),
    ))
}
