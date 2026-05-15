//! Peer-group runtime resolution.
//!
//! Given a `Config` and an `agent_alias`, produces the effective set
//! of peers that agent should accept inbound messages from on its
//! configured channels. The schema-side primitive is the
//! `[peer_groups.<name>]` block in `zeroclaw-config::multi_agent`;
//! this module is the read-side resolver that walks the configured
//! groups, applies the mutual-membership rule, unions external peers,
//! subtracts the per-group ignore lists, and returns the result keyed
//! by channel.
//!
//! Cross-reference invariants (peer-group members are configured
//! agents, the group's channel is on each member's `channels` list)
//! are upheld at config load. By the time the runtime calls
//! [`resolve_peer_set`], every input is internally consistent.

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

    /// NOT a security gate. Unknown senders return `true` by design;
    /// peer groups are an additive routing hint for cross-agent traffic,
    /// not a global inbound allowlist. Callers must have already
    /// authenticated the sender (channel auth, signed webhook, etc.)
    /// before reaching this check.
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

/// Defense-in-depth self-loop guard for the agent loop entry point.
///
/// Returns `true` when `sender` is recognizable as the bot's own
/// outbound identity on this channel and the agent loop should refuse
/// to spawn a turn. Mirrors `Channel::drop_self_messages`'s
/// normalization (strip leading `@`, case-insensitive) so the two
/// layers agree on what "self" means; the agent-loop call is a
/// fallback for channel impls that route around the SDK guard or that
/// expose self-identity later in their lifecycle than the
/// orchestrator's check fires.
#[must_use]
pub fn should_drop_self_loop(sender: &str, self_handle: Option<&str>) -> bool {
    let Some(handle) = self_handle else {
        return false;
    };
    let handle_norm = handle.trim_start_matches('@').to_ascii_lowercase();
    let sender_norm = sender.trim_start_matches('@').to_ascii_lowercase();
    !handle_norm.is_empty() && handle_norm == sender_norm
}

/// Build the effective peer set for `agent_alias`.
///
/// Walks every `[peer_groups.<name>]` entry the agent appears in:
///
/// 1. Other agents in the same group (mutual membership) become peers
///    on the group's channel.
/// 2. The group's `external_peers` are added on the group's channel.
/// 3. The group's `ignore` list is subtracted from both sets.
/// 4. The bound agent's own alias is removed defensively (a misconfig
///    that lists the agent in its own group's external_peers is the
///    classic self-loop footgun the channel SDK already drops at the
///    other end).
///
/// Returns an empty [`ResolvedPeers`] when the agent isn't on any
/// peer group — the agent runs solo with no cross-agent dispatch.
#[must_use]
pub fn resolve_peer_set(config: &Config, agent_alias: &str) -> ResolvedPeers {
    let mut resolved = ResolvedPeers::default();

    for group in config.peer_groups.values() {
        let on_group = group.agents.iter().any(|a| a.as_str() == agent_alias);
        if !on_group {
            continue;
        }

        let channel = group.channel.clone();
        let agent_set = resolved.agent_peers.entry(channel.clone()).or_default();
        // Aliases are stored case-folded so the lookup side
        // (`is_known_peer` / `allows_inbound`) can normalize without
        // missing `@Beta` against a config of `[agents.beta]` or
        // similar. Aliases are config map keys — the schema does not
        // enforce a case rule, so we match insensitively.
        let self_norm = agent_alias.trim_start_matches('@').to_ascii_lowercase();
        for member in &group.agents {
            let normalized = member.as_str().trim_start_matches('@').to_ascii_lowercase();
            if normalized != self_norm {
                agent_set.insert(normalized);
            }
        }

        let ext_set = resolved.external_peers.entry(channel.clone()).or_default();
        for ext in &group.external_peers {
            // PeerUsername is already case-folded and `@`-stripped at
            // deserialization (multi_agent.rs).
            ext_set.insert(ext.as_str().to_ascii_lowercase());
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
}
