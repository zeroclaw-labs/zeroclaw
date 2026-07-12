//! Runtime resolution of tool parameter option domains.
//!
//! Tools declare *which* domain a parameter draws from
//! (`Tool::param_domains`); this module resolves those domains against
//! live config so authoring surfaces render real selectable choices.
//! Cascading domains (e.g. `PeerTargets` narrowing on a chosen
//! `channel`) receive the partially filled argument object.

use std::collections::BTreeSet;

use zeroclaw_api::tool::{OptionDomain, OptionEntry, Tool};
use zeroclaw_config::schema::Config;

use crate::peers::resolve_peer_set;

/// Resolve the selectable values for one domain.
///
/// `agent_alias` scopes agent-relative domains (peer targets, channel
/// refs). `partial_args` carries sibling arguments already chosen in the
/// editor so dependent domains can narrow; resolution degrades to the
/// unfiltered union when the driving argument is absent or non-literal.
/// `tools` feeds `ToolNames`; pass the active registry slice.
pub fn resolve_options(
    domain: OptionDomain,
    config: &Config,
    agent_alias: &str,
    partial_args: &serde_json::Value,
    tools: &[&dyn Tool],
) -> Vec<OptionEntry> {
    match domain {
        OptionDomain::ChannelRefs => channel_refs(config, agent_alias),
        OptionDomain::PeerTargets => peer_targets(config, agent_alias, partial_args),
        OptionDomain::PeerGroups => peer_groups(config, agent_alias),
        OptionDomain::AgentAliases => agent_aliases(config),
        OptionDomain::ToolNames => tool_names(tools),
        OptionDomain::MemoryCategories => memory_categories(),
    }
}

fn channel_refs(config: &Config, agent_alias: &str) -> Vec<OptionEntry> {
    let agent_channels: Option<BTreeSet<String>> = config
        .agents
        .get(agent_alias)
        .map(|a| a.channels.iter().map(|c| c.to_string()).collect());
    config
        .channels_by_alias()
        .into_iter()
        .filter(|info| {
            agent_channels.as_ref().is_none_or(|set| {
                let dotted = format!("{}.{}", info.channel_type, info.alias);
                set.contains(&dotted) || set.contains(&info.channel_type)
            })
        })
        .map(|info| {
            let dotted = format!("{}.{}", info.channel_type, info.alias);
            let entry = OptionEntry::new(dotted);
            if info.enabled {
                entry
            } else {
                entry.with_hint("disabled")
            }
        })
        .collect()
}

/// Channel key the `channel` argument narrows peer targets by. Accepts a
/// dotted ref (`telegram.work`) or bare type (`telegram`); peer groups
/// bind on either form, so both the exact key and its type prefix match.
fn channel_filter_keys(partial_args: &serde_json::Value) -> Option<(String, String)> {
    let raw = partial_args.get("channel")?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let channel_type = raw.split('.').next().unwrap_or(raw).to_string();
    Some((raw.to_string(), channel_type))
}

fn peer_targets(
    config: &Config,
    agent_alias: &str,
    partial_args: &serde_json::Value,
) -> Vec<OptionEntry> {
    let resolved = resolve_peer_set(config, agent_alias);
    let filter = channel_filter_keys(partial_args);
    let keep = |channel_key: &str| {
        filter
            .as_ref()
            .is_none_or(|(exact, channel_type)| channel_key == exact || channel_key == channel_type)
    };

    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();

    for (channel_key, peers) in &resolved.agent_peers {
        if !keep(channel_key) {
            continue;
        }
        for peer in peers {
            if seen.insert(peer.clone()) {
                entries.push(OptionEntry::new(peer.clone()).with_hint("peer agent"));
            }
        }
    }
    for (channel_key, peers) in &resolved.external_peers {
        if !keep(channel_key) {
            continue;
        }
        for peer in peers {
            let handle = format!("@{peer}");
            if seen.insert(handle.clone()) {
                entries.push(OptionEntry::new(handle).with_hint("external peer"));
            }
        }
    }
    entries
}

fn peer_groups(config: &Config, agent_alias: &str) -> Vec<OptionEntry> {
    let mut entries: Vec<OptionEntry> = config
        .peer_groups
        .iter()
        .filter(|(_, group)| group.agents.iter().any(|a| a.as_str() == agent_alias))
        .map(|(name, group)| {
            let members = group.agents.len() + group.external_peers.len();
            OptionEntry::new(name.clone()).with_hint(format!("group: {members} members"))
        })
        .collect();
    entries.sort_by(|a, b| a.value.cmp(&b.value));
    entries
}

fn agent_aliases(config: &Config) -> Vec<OptionEntry> {
    let mut entries: Vec<OptionEntry> = config
        .agents
        .keys()
        .map(|alias| OptionEntry::new(alias.clone()))
        .collect();
    entries.sort_by(|a, b| a.value.cmp(&b.value));
    entries
}

fn tool_names(tools: &[&dyn Tool]) -> Vec<OptionEntry> {
    let mut entries: Vec<OptionEntry> = tools
        .iter()
        .map(|t| OptionEntry::new(t.name()).with_hint(truncate(t.description(), 60)))
        .collect();
    entries.sort_by(|a, b| a.value.cmp(&b.value));
    entries
}

fn memory_categories() -> Vec<OptionEntry> {
    use zeroclaw_memory::traits::MemoryCategory;
    [
        MemoryCategory::Core,
        MemoryCategory::Daily,
        MemoryCategory::Conversation,
    ]
    .iter()
    .map(|c| OptionEntry::new(c.to_string()))
    .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};

    fn config_with_groups() -> Config {
        let mut config = Config::default();
        config.peer_groups.insert(
            "ops".into(),
            PeerGroupConfig {
                channel: "discord.main".into(),
                agents: vec![AgentAlias::new("aria"), AgentAlias::new("zeph")],
                external_peers: vec![PeerUsername::new("@operator")],
                ..PeerGroupConfig::default()
            },
        );
        config.peer_groups.insert(
            "research".into(),
            PeerGroupConfig {
                channel: "telegram".into(),
                agents: vec![AgentAlias::new("aria"), AgentAlias::new("scout")],
                ..PeerGroupConfig::default()
            },
        );
        config
    }

    #[test]
    fn peer_targets_unfiltered_returns_union() {
        let config = config_with_groups();
        let entries = resolve_options(
            OptionDomain::PeerTargets,
            &config,
            "aria",
            &serde_json::json!({}),
            &[],
        );
        let values: Vec<&str> = entries.iter().map(|e| e.value.as_str()).collect();
        assert!(values.contains(&"zeph"));
        assert!(values.contains(&"scout"));
        assert!(values.contains(&"@operator"));
    }

    #[test]
    fn peer_targets_cascade_narrows_on_channel() {
        let config = config_with_groups();
        let entries = resolve_options(
            OptionDomain::PeerTargets,
            &config,
            "aria",
            &serde_json::json!({"channel": "discord.main"}),
            &[],
        );
        let values: Vec<&str> = entries.iter().map(|e| e.value.as_str()).collect();
        assert!(values.contains(&"zeph"));
        assert!(values.contains(&"@operator"));
        assert!(!values.contains(&"scout"));
    }

    #[test]
    fn peer_targets_bare_type_filter_matches_typed_group() {
        let config = config_with_groups();
        let entries = resolve_options(
            OptionDomain::PeerTargets,
            &config,
            "aria",
            &serde_json::json!({"channel": "telegram.prod"}),
            &[],
        );
        // group bound to bare "telegram" must match a dotted telegram ref
        // via its type prefix
        let values: Vec<&str> = entries.iter().map(|e| e.value.as_str()).collect();
        assert!(values.contains(&"scout"));
        assert!(!values.contains(&"zeph"));
    }

    #[test]
    fn peer_groups_scoped_to_member_agent() {
        let config = config_with_groups();
        let mine = resolve_options(
            OptionDomain::PeerGroups,
            &config,
            "scout",
            &serde_json::json!({}),
            &[],
        );
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].value, "research");
        assert_eq!(mine[0].hint, "group: 2 members");
    }

    #[test]
    fn agent_aliases_walks_config() {
        let mut config = Config::default();
        config.agents.insert("beta".into(), Default::default());
        config.agents.insert("alpha".into(), Default::default());
        let entries = resolve_options(
            OptionDomain::AgentAliases,
            &config,
            "",
            &serde_json::json!({}),
            &[],
        );
        let values: Vec<&str> = entries.iter().map(|e| e.value.as_str()).collect();
        assert_eq!(values, vec!["alpha", "beta"]);
    }
}
