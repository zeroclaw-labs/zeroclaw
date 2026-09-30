//! A2A discovery cards: the catalog card and per-alias agent cards.
//!
//! The core builds these from config so both the gateway's well-known routes
//! and the `a2a/identity` RPC method serve the same card. The gateway may pass
//! an [`AdvertisedGatewayEndpoint`] when it was started with a host or port
//! override; the core, which does not yet learn the gateway's listener
//! address, builds from config alone.

use zeroclaw_config::schema::Config;

use crate::skills::SkillsService;

pub use zeroclaw_api::a2a_wire::{AgentCapabilities, AgentCard, AgentInterface, AgentSkill};

/// A2A protocol version advertised on per-alias interfaces.
pub const A2A_PROTOCOL_VERSION: &str = "1.0";
/// JSON-RPC is the spec-mandated baseline transport binding.
pub const A2A_PROTOCOL_BINDING: &str = "JSONRPC";
pub const CATALOG_CARD_PATH: &str = "/.well-known/agents-card.json";

/// Runtime gateway endpoint used for A2A advertisement when the operator starts
/// the gateway with CLI host/port overrides. This is created from the listener
/// inputs at route construction time; persistent config remains the source of
/// truth for config-defined URLs and explicit A2A advertisement overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedGatewayEndpoint {
    host: String,
    port: u16,
}

impl AdvertisedGatewayEndpoint {
    #[must_use]
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

fn advertised_base(config: &Config, endpoint: Option<&AdvertisedGatewayEndpoint>) -> String {
    let server = &config.a2a.server;
    let configured = server.public_base_url.trim();
    if !configured.is_empty() {
        return configured.trim_end_matches('/').to_string();
    }
    let host = server
        .bind
        .clone()
        .or_else(|| endpoint.map(|endpoint| endpoint.host.clone()))
        .unwrap_or_else(|| config.gateway.host.clone());
    let port = server
        .port
        .or_else(|| endpoint.map(|endpoint| endpoint.port))
        .unwrap_or(config.gateway.port);
    format!("http://{host}:{port}")
}

/// Per-alias A2A base path under the advertised origin.
fn alias_base_path(alias: &str) -> String {
    format!("/a2a/{alias}")
}

/// Build the ZeroClaw discovery catalog card served at the origin root. Lists
/// every published alias as a skill-less entry pointing at its per-alias card
/// and endpoint. This is a catalog, not a runnable agent: it advertises the
/// `catalog` interface and carries no skills of its own.
#[must_use]
pub fn build_catalog_card(config: &Config) -> AgentCard {
    build_catalog_card_with_endpoint(config, None)
}

#[must_use]
pub fn build_catalog_card_with_endpoint(
    config: &Config,
    endpoint: Option<&AdvertisedGatewayEndpoint>,
) -> AgentCard {
    let base = advertised_base(config, endpoint);
    let published = published_aliases(config);

    let mut supported_interfaces = Vec::with_capacity(published.len() + 1);
    supported_interfaces.push(AgentInterface {
        url: format!("{base}{CATALOG_CARD_PATH}"),
        protocol_binding: "catalog".to_string(),
        tenant: None,
        protocol_version: A2A_PROTOCOL_VERSION.to_string(),
    });
    for alias in &published {
        supported_interfaces.push(AgentInterface {
            url: format!("{base}{}", alias_base_path(alias)),
            protocol_binding: A2A_PROTOCOL_BINDING.to_string(),
            tenant: None,
            protocol_version: A2A_PROTOCOL_VERSION.to_string(),
        });
    }

    let mut skills = Vec::new();
    for alias in &published {
        for mut skill in exposed_skills(config, alias) {
            skill.id = format!("{alias}/{}", skill.id);
            skill.tags.push(alias.clone());
            skills.push(skill);
        }
    }

    AgentCard {
        name: "ZeroClaw agents".to_string(),
        description: "Discovery catalog enumerating published A2A agents on \
                      this ZeroClaw install. Not a runnable agent; each entry \
                      below serves its own A2A card and endpoint. Skills are \
                      aggregated from the published agents, each tagged with \
                      its owning alias."
            .to_string(),
        supported_interfaces,
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: AgentCapabilities {
            streaming: Some(false),
            push_notifications: Some(false),
            extended_agent_card: Some(false),
        },
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        skills,
    }
}

/// Build a spec-conforming per-alias agent card, or `None` when the alias is
/// unknown or not published. Skills resolve from the alias's bundles and
/// narrow through `exposed_skills`.
#[must_use]
pub fn build_agent_card(config: &Config, alias: &str) -> Option<AgentCard> {
    build_agent_card_with_endpoint(config, alias, None)
}

#[must_use]
pub fn build_agent_card_with_endpoint(
    config: &Config,
    alias: &str,
    endpoint: Option<&AdvertisedGatewayEndpoint>,
) -> Option<AgentCard> {
    let agent = config.agents.get(alias)?;
    if !agent.enabled || !agent.a2a.published {
        return None;
    }

    let base = advertised_base(config, endpoint);
    let endpoint = format!("{base}{}", alias_base_path(alias));

    AgentCard {
        name: alias.to_string(),
        description: agent_description(config, alias),
        supported_interfaces: vec![AgentInterface {
            url: endpoint,
            protocol_binding: A2A_PROTOCOL_BINDING.to_string(),
            tenant: None,
            protocol_version: A2A_PROTOCOL_VERSION.to_string(),
        }],
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: AgentCapabilities {
            streaming: Some(false),
            push_notifications: Some(false),
            extended_agent_card: Some(false),
        },
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        skills: exposed_skills(config, alias),
    }
    .into()
}

/// Aliases that are both enabled and A2A-published, in stable sorted order.
pub fn published_aliases(config: &Config) -> Vec<String> {
    let mut out: Vec<String> = config
        .agents
        .iter()
        .filter(|(_, agent)| agent.enabled && agent.a2a.published)
        .map(|(alias, _)| alias.clone())
        .collect();
    out.sort();
    out
}

fn agent_description(config: &Config, alias: &str) -> String {
    if let Some(desc) = identity_description(config, alias) {
        return desc;
    }
    format!("ZeroClaw agent '{alias}'.")
}

/// Resolve a one-line description from the alias identity document, or `None`
/// when there is no usable line. Reuses the runtime AIEOS loader so the
/// gateway and the agent system prompt read identity through the same path.
fn identity_description(config: &Config, alias: &str) -> Option<String> {
    let agent = config.agents.get(alias)?;
    let workspace_dir = config.agent_workspace_dir(alias);
    let aieos = crate::identity::load_aieos_identity(&agent.identity, &workspace_dir)
        .ok()
        .flatten()?;
    let identity = aieos.identity?;
    let line = identity
        .bio
        .filter(|b| !b.trim().is_empty())
        .or_else(|| identity.names.and_then(identity_name_line))?;
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// Build a name line from identity `names`, preferring the fullest form.
fn identity_name_line(names: crate::identity::Names) -> Option<String> {
    if let Some(full) = names.full.filter(|s| !s.trim().is_empty()) {
        return Some(full);
    }
    let joined = [names.first, names.last]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !joined.is_empty() {
        return Some(joined);
    }
    names.nickname.filter(|s| !s.trim().is_empty())
}

/// Resolve the alias's exposed skills: the resolved bundle skill set, after the
/// owning bundle's include/exclude filter, narrowed by `exposed_skills`. An
/// empty filter advertises no skills. Skill ids that do not resolve to a real,
/// admitted skill are dropped (bundles are canonical).
pub fn exposed_skills(config: &Config, alias: &str) -> Vec<AgentSkill> {
    let agent = match config.agents.get(alias) {
        Some(a) => a,
        None => return Vec::new(),
    };
    if agent.a2a.exposed_skills.is_empty() {
        return Vec::new();
    }

    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let resolved = match service.list_skills(None) {
        Ok(skills) => skills,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for wanted in &agent.a2a.exposed_skills {
        if let Some(summary) = resolved.iter().find(|s| {
            s.r#ref.name() == wanted
                && agent.skill_bundles.iter().any(|b| b == s.r#ref.bundle())
                && config
                    .skill_bundles
                    .get(s.r#ref.bundle())
                    .is_some_and(|bundle| bundle.admits_skill(s.r#ref.name()))
        }) {
            let mut tags = vec![summary.r#ref.bundle().to_string()];
            if let Some(category) = &summary.frontmatter.category
                && !category.is_empty()
            {
                tags.push(category.clone());
            }
            out.push(AgentSkill {
                id: summary.r#ref.name().to_string(),
                name: summary.frontmatter.name.clone(),
                description: summary.frontmatter.description.clone(),
                tags,
            });
        }
    }
    out
}
