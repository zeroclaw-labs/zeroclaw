//! Peer-group runtime resolution.

use std::collections::{BTreeMap, BTreeSet};
use zeroclaw_config::schema::Config;

/// Effective peer set for one agent, keyed by channel type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedPeers {
    /// Channel type → peer-agent aliases (bound agent excluded).
    pub agent_peers: BTreeMap<String, BTreeSet<String>>,
    /// Channel type → external-peer usernames (case-folded).
    pub external_peers: BTreeMap<String, BTreeSet<String>>,
}

impl ResolvedPeers {
    /// Whether the bound agent recognizes `target` as a peer on a
    /// channel of `channel_type`. Outbound gate: unknown returns false.
    #[must_use]
    pub fn is_known_peer(&self, channel_type: &str, target: &str) -> bool {
        let normalized = target.trim_start_matches('@').to_ascii_lowercase();
        if let Some(agent_set) = self.agent_peers.get(channel_type)
            && agent_set.contains(&normalized)
        {
            return true;
        }
        if let Some(ext_set) = self.external_peers.get(channel_type)
            && ext_set.contains(&normalized)
        {
            return true;
        }
        false
    }

    #[must_use]
    pub fn allows_inbound(&self, channel_type: &str, origin: &str) -> bool {
        let normalized = origin.trim_start_matches('@').to_ascii_lowercase();
        if let Some(agent_set) = self.agent_peers.get(channel_type)
            && agent_set.contains(&normalized)
        {
            return true;
        }
        if let Some(ext_set) = self.external_peers.get(channel_type)
            && ext_set.contains(&normalized)
        {
            return true;
        }
        true
    }
}

#[must_use]
pub fn should_drop_self_loop(sender: &str, self_handle: Option<&str>) -> bool {
    let Some(handle) = self_handle else {
        return false;
    };
    let handle_norm = handle.trim_start_matches('@').to_ascii_lowercase();
    let sender_norm = sender.trim_start_matches('@').to_ascii_lowercase();
    !handle_norm.is_empty() && handle_norm == sender_norm
}

#[must_use]
pub fn resolve_peer_set(config: &Config, agent_alias: &str) -> ResolvedPeers {
    let mut resolved = ResolvedPeers::default();

    for group in config.peer_groups.values() {
        let on_group = group.agents.iter().any(|a| a.as_str() == agent_alias);
        if !on_group {
            continue;
        }

        let channel = group.channel.to_string();
        let agent_set = resolved.agent_peers.entry(channel.clone()).or_default();
        let self_norm = agent_alias.trim_start_matches('@').to_ascii_lowercase();
        for member in &group.agents {
            let normalized = member.as_str().trim_start_matches('@').to_ascii_lowercase();
            if normalized != self_norm {
                agent_set.insert(normalized);
            }
        }

        let ext_set = resolved.external_peers.entry(channel.clone()).or_default();
        for ext in &group.external_peers {
            // Match the lookup side (`is_known_peer` / `allows_inbound`):
            // channel-native usernames may be configured with or without a
            // leading `@`, and callers may pass either form.
            ext_set.insert(ext.as_str().trim_start_matches('@').to_ascii_lowercase());
        }

        for ignored in &group.ignore {
            let needle = ignored
                .as_str()
                .trim_start_matches('@')
                .to_ascii_lowercase();
            ext_set.remove(&needle);
            agent_set.remove(&needle);
        }
    }

    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_drop_self_loop_returns_false_when_handle_unknown() {
        assert!(!should_drop_self_loop("@anyone", None));
    }

    #[test]
    fn should_drop_self_loop_matches_normalized_handle() {
        assert!(should_drop_self_loop("@my_bot", Some("@my_bot")));
        assert!(should_drop_self_loop("@MY_BOT", Some("my_bot")));
        assert!(should_drop_self_loop("my_bot", Some("@My_Bot")));
        assert!(!should_drop_self_loop("@other_bot", Some("@my_bot")));
    }

    #[test]
    fn should_drop_self_loop_ignores_empty_handle_after_normalization() {
        // A handle of "@" (empty after stripping the @) must not match
        // every inbound; the guard only fires on a real handle.
        assert!(!should_drop_self_loop("@anyone", Some("@")));
    }

    #[test]
    fn resolve_peer_set_normalizes_external_peer_handles_for_lookup() {
        use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};
        use zeroclaw_config::schema::Config;

        let mut config = Config::default();
        config.peer_groups.insert(
            "ops".to_string(),
            PeerGroupConfig {
                channel: "telegram".into(),
                agents: vec![AgentAlias::new("aa")],
                external_peers: vec![PeerUsername::new("@Operator")],
                ..PeerGroupConfig::default()
            },
        );

        let resolved = resolve_peer_set(&config, "aa");

        assert!(resolved.is_known_peer("telegram", "operator"));
        assert!(resolved.is_known_peer("telegram", "@operator"));
        assert!(resolved.allows_inbound("telegram", "operator"));
        assert!(resolved.allows_inbound("telegram", "@operator"));
    }
}
