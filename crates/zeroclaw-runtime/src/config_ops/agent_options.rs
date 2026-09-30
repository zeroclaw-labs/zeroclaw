//! Every alias-reference list an agent form needs, derived from the live
//! config. Shared by `GET /api/config/agent-options` and RPC
//! `config/agent-options`.

use serde::{Deserialize, Serialize};

/// All alias-reference choices an agent form needs, in one round-trip.
/// Channels and model model_providers are returned in dotted form
/// (`telegram.default`, `anthropic.work`); the bundle/profile/namespace
/// lists are bare HashMap keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AgentOptionsResponse {
    pub channels: Vec<String>,
    /// Distinct channel types with at least one configured alias —
    /// `["discord", "telegram"]`. Source for peer-group channel picker.
    pub channel_types: Vec<String>,
    pub model_providers: Vec<String>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    pub skill_bundles: Vec<String>,
    pub knowledge_bundles: Vec<String>,
    pub mcp_bundles: Vec<String>,
    pub agents: Vec<String>,
}

pub fn build_agent_options(cfg: &zeroclaw_config::schema::Config) -> AgentOptionsResponse {
    use zeroclaw_config::traits::AliasSource;

    let channels = cfg.resolve_alias_source(AliasSource::Channels);
    let mut channel_types: Vec<String> = channels
        .iter()
        .filter_map(|d| d.split_once('.').map(|(t, _)| t.to_string()))
        .collect();
    channel_types.sort();
    channel_types.dedup();

    AgentOptionsResponse {
        channels,
        channel_types,
        model_providers: cfg.resolve_alias_source(AliasSource::ModelProviders),
        risk_profiles: cfg.resolve_alias_source(AliasSource::RiskProfiles),
        runtime_profiles: cfg.resolve_alias_source(AliasSource::RuntimeProfiles),
        skill_bundles: cfg.resolve_alias_source(AliasSource::SkillBundles),
        knowledge_bundles: cfg.resolve_alias_source(AliasSource::KnowledgeBundles),
        mcp_bundles: cfg.resolve_alias_source(AliasSource::McpBundles),
        agents: cfg.resolve_alias_source(AliasSource::Agents),
    }
}
