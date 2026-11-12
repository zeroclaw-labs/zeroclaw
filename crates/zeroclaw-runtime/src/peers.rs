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
    /// Normalized `ignore` names (and `"*"`) keyed by the group's `channel`.
    /// Stored separately so a type-wide grant can stay visible on the bare
    /// type while still being denied on a dotted alias.
    ignores: BTreeMap<String, BTreeSet<String>>,
}

impl ResolvedPeers {
    /// Whether the bound agent recognizes `target` as a peer on a
    /// channel of `channel_type`. Outbound gate: unknown returns false.
    ///
    /// A dotted alias (`discord.main`) also sees type-wide grants and
    /// denies on the bare type (`discord`). `ignore` wins, including
    /// `ignore = ["*"]`.
    #[must_use]
    pub fn is_known_peer(&self, channel_type: &str, target: &str) -> bool {
        let normalized = normalize_peer(target);
        if self.is_ignored(channel_type, &normalized) {
            return false;
        }
        self.is_granted(channel_type, &normalized)
    }

    /// Whether `target` is denied on `channel_type` by any in-scope group.
    #[must_use]
    pub fn is_ignored(&self, channel_type: &str, target: &str) -> bool {
        let normalized = normalize_peer(target);
        for key in policy_keys(channel_type) {
            if let Some(set) = self.ignores.get(key)
                && (set.contains("*") || set.contains(&normalized))
            {
                return true;
            }
        }
        false
    }

    fn is_granted(&self, channel_type: &str, normalized: &str) -> bool {
        for key in policy_keys(channel_type) {
            if let Some(agent_set) = self.agent_peers.get(key)
                && agent_set.contains(normalized)
            {
                return true;
            }
            if let Some(ext_set) = self.external_peers.get(key)
                && ext_set.contains(normalized)
            {
                return true;
            }
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
fn normalize_peer(name: &str) -> String {
    name.trim_start_matches('@').to_ascii_lowercase()
}

/// Keys whose grants and denies apply when looking up `channel_type`.
/// A dotted alias inherits the bare type; a bare type does not inherit
/// instance-scoped groups.
fn policy_keys(channel_type: &str) -> Vec<&str> {
    match channel_type.split_once('.') {
        Some((ty, _)) if ty != channel_type => vec![channel_type, ty],
        _ => vec![channel_type],
    }
}

pub fn resolve_peer_set(config: &Config, agent_alias: &str) -> ResolvedPeers {
    let mut resolved = ResolvedPeers::default();

    for group in config.peer_groups.values() {
        let on_group = group.agents.iter().any(|a| a.as_str() == agent_alias);
        if !on_group {
            continue;
        }

        let channel = group.channel.to_string();
        let agent_set = resolved.agent_peers.entry(channel.clone()).or_default();
        let self_norm = normalize_peer(agent_alias);
        for member in &group.agents {
            let normalized = normalize_peer(member.as_str());
            if normalized != self_norm {
                agent_set.insert(normalized);
            }
        }

        let ext_set = resolved.external_peers.entry(channel.clone()).or_default();
        for ext in &group.external_peers {
            // Match the lookup side (`is_known_peer` / `allows_inbound`):
            // channel-native usernames may be configured with or without a
            // leading `@`, and callers may pass either form.
            ext_set.insert(normalize_peer(ext.as_str()));
        }

        let ignore_set = resolved.ignores.entry(channel).or_default();
        for ignored in &group.ignore {
            ignore_set.insert(normalize_peer(ignored.as_str()));
        }
    }

    // Subtract in-scope denies from the stored grant sets so map
    // consumers (peer-target suggestions, the sender peer map) do not
    // advertise a peer that `is_known_peer` would reject for that key.
    // Type-wide grants stay on the bare-type key so other aliases can
    // still see them; dotted lookups consult `ignores` directly.
    apply_ignores_to_grant_sets(&mut resolved);
    resolved
}

fn apply_ignores_to_grant_sets(resolved: &mut ResolvedPeers) {
    let keys: Vec<String> = resolved
        .agent_peers
        .keys()
        .chain(resolved.external_peers.keys())
        .cloned()
        .collect();
    for key in keys {
        let denied_all = policy_keys(&key).into_iter().any(|policy_key| {
            resolved
                .ignores
                .get(policy_key)
                .is_some_and(|set| set.contains("*"))
        });
        if denied_all {
            if let Some(set) = resolved.agent_peers.get_mut(&key) {
                set.clear();
            }
            if let Some(set) = resolved.external_peers.get_mut(&key) {
                set.clear();
            }
            continue;
        }
        let denied: BTreeSet<String> = policy_keys(&key)
            .into_iter()
            .filter_map(|policy_key| resolved.ignores.get(policy_key))
            .flatten()
            .cloned()
            .collect();
        if let Some(set) = resolved.agent_peers.get_mut(&key) {
            set.retain(|peer| !denied.contains(peer));
        }
        if let Some(set) = resolved.external_peers.get_mut(&key) {
            set.retain(|peer| !denied.contains(peer));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};
    use zeroclaw_config::schema::Config;

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

    fn group(
        channel: &str,
        agents: &[&str],
        external_peers: &[&str],
        ignore: &[&str],
    ) -> PeerGroupConfig {
        PeerGroupConfig {
            channel: channel.into(),
            agents: agents.iter().copied().map(AgentAlias::new).collect(),
            external_peers: external_peers
                .iter()
                .copied()
                .map(PeerUsername::new)
                .collect(),
            ignore: ignore.iter().copied().map(PeerUsername::new).collect(),
            ..PeerGroupConfig::default()
        }
    }

    fn config_with_allow_and_deny(
        allow_channel: &str,
        deny_channel: &str,
        deny_ignore: &[&str],
        allow_first: bool,
    ) -> Config {
        let mut config = Config::default();
        let allow = (
            "allow".to_string(),
            group(allow_channel, &["assistant"], &["alice"], &[]),
        );
        let deny = (
            "deny".to_string(),
            group(deny_channel, &["assistant"], &[], deny_ignore),
        );
        if allow_first {
            config.peer_groups.insert(allow.0, allow.1);
            config.peer_groups.insert(deny.0, deny.1);
        } else {
            config.peer_groups.insert(deny.0, deny.1);
            config.peer_groups.insert(allow.0, allow.1);
        }
        config
    }

    /// #12283: `ignore` must win over a grant on the same channel key,
    /// independent of `HashMap` iteration order.
    #[test]
    fn resolve_peer_set_same_channel_ignore_wins_regardless_of_group_order() {
        // HashMap iteration order is not insertion order, so build many
        // fresh maps; the deny must win in every run, not merely on average.
        for i in 0..32 {
            let allow_first = i % 2 == 0;
            let config =
                config_with_allow_and_deny("discord.main", "discord.main", &["alice"], allow_first);
            let resolved = resolve_peer_set(&config, "assistant");
            assert!(
                !resolved.is_known_peer("discord.main", "alice"),
                "iteration {i}, allow_first={allow_first}: ignore on the same channel must win"
            );
            assert!(!resolved.is_known_peer("discord.main", "@Alice"));
        }
    }

    /// #12283: a type-wide `ignore` on `discord` applies to `discord.main`.
    #[test]
    fn resolve_peer_set_type_wide_ignore_applies_to_dotted_alias() {
        for allow_first in [true, false] {
            let config =
                config_with_allow_and_deny("discord.main", "discord", &["alice"], allow_first);
            let resolved = resolve_peer_set(&config, "assistant");
            assert!(
                !resolved.is_known_peer("discord.main", "alice"),
                "allow_first={allow_first}: type-wide ignore must deny the dotted alias"
            );
        }
    }

    /// #12283: `ignore = ["*"]` denies every peer on the matching channel.
    #[test]
    fn resolve_peer_set_wildcard_ignore_denies_everyone() {
        for allow_first in [true, false] {
            let config =
                config_with_allow_and_deny("discord.main", "discord.main", &["*"], allow_first);
            let resolved = resolve_peer_set(&config, "assistant");
            assert!(
                !resolved.is_known_peer("discord.main", "alice"),
                "allow_first={allow_first}: wildcard ignore must deny every peer"
            );
        }
    }

    #[test]
    fn resolve_peer_set_instance_ignore_does_not_clear_type_wide_lookup() {
        let config = config_with_allow_and_deny("discord", "discord.main", &["alice"], true);
        let resolved = resolve_peer_set(&config, "assistant");
        // Instance-scoped ignore applies to that alias...
        assert!(!resolved.is_known_peer("discord.main", "alice"));
        // ...but a type-wide grant remains visible on the bare type (other aliases).
        assert!(resolved.is_known_peer("discord", "alice"));
    }

    #[test]
    fn resolve_peer_set_ignore_is_channel_scoped() {
        let mut config = Config::default();
        config.peer_groups.insert(
            "telegram_allow".to_string(),
            group("telegram", &["assistant"], &["alice"], &[]),
        );
        config.peer_groups.insert(
            "discord_deny".to_string(),
            group("discord.main", &["assistant"], &[], &["alice"]),
        );
        let resolved = resolve_peer_set(&config, "assistant");
        assert!(resolved.is_known_peer("telegram", "alice"));
        assert!(!resolved.is_known_peer("discord.main", "alice"));
    }
}
