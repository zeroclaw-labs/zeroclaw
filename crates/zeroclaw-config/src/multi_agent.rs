//! Multi-agent runtime types: alias newtypes, access-mode enum, peer
//! external entries, and the nested config structs that wire into
//! [`crate::schema::AliasedAgentConfig`] and [`crate::schema::Config`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use zeroclaw_macros::Configurable;

crate::define_provider_ref!(AgentAlias, "agents");
crate::define_provider_ref!(PeerGroupName, "peer_groups");
crate::define_provider_ref!(PeerUsername, "channels.peers");

/// A cross-agent filesystem grant from a workspace allowlist entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    /// Read access only. Cross-agent `file_read` is permitted; writes are not.
    Read,
    /// Write access only. Cross-agent `file_write` is permitted; reads are not.
    Write,
    /// Both read and write. The agent can `file_read` and `file_write` against
    /// the target's workspace.
    ReadWrite,
}

/// Selects the memory backend used by an agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MemoryBackendKind {
    /// No memory backend. Recall returns empty; stores are no-ops.
    None,
    /// Embedded SQLite (`crates/zeroclaw-memory/src/sqlite.rs`). Default for
    /// new installs because every supported platform can run it without
    /// extra services.
    #[default]
    Sqlite,
    /// PostgreSQL with optional pgvector
    /// (`crates/zeroclaw-memory/src/postgres.rs`, feature `memory-postgres`).
    Postgres,
    /// Qdrant vector store (`crates/zeroclaw-memory/src/qdrant.rs`).
    Qdrant,
    /// Markdown files in the agent's workspace
    /// (`crates/zeroclaw-memory/src/markdown.rs`).
    Markdown,
    /// Hybrid local SQLite + external Lucid CLI
    /// (`crates/zeroclaw-memory/src/lucid.rs`).
    Lucid,
}

/// Per-agent workspace and cross-agent access configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent_workspace"]
#[serde(default)]
pub struct AgentWorkspaceConfig {
    /// Optional explicit workspace path. `None` = derive from
    /// `<install>/agents/<alias>/workspace/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Cross-agent workspace allowlist. An empty map grants no sibling access.
    pub access: BTreeMap<AgentAlias, AccessMode>,
    /// Escape hatch: when `true`, the agent can read or write anywhere
    /// the host filesystem permits. Off by default; flipping this on is
    /// auditable.
    pub unrestricted_filesystem: bool,
    /// Cross-agent memory allowlist. An empty list grants access only to local memory.
    pub read_memory_from: Vec<AgentAlias>,
}

/// Per-agent memory backend selection and its persistence contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent_memory"]
#[serde(default)]
pub struct AgentMemoryConfig {
    /// The backend kind this agent uses. Defaults to `Sqlite` for new
    /// agents; once an agent has on-disk data the value is locked.
    pub backend: MemoryBackendKind,
}

/// Preferred output modality for a peer group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OutputModality {
    /// Always reply in kind — voice note if user sent voice, text otherwise.
    #[default]
    Mirror,
    /// Always deliver via TTS as a voice note, regardless of input modality.
    /// Applies to proactive messages (cron, announces) as well as replies.
    Voice,
    /// Always deliver as text, even if user sent a voice note.
    Text,
}

/// `[peer_groups.<name>]` — mutual-opt-in peer group on a channel type.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "peer_group"]
#[serde(default)]
pub struct PeerGroupConfig {
    /// Either a channel type (`"telegram"`) or a dotted channel alias
    /// (`"telegram.work"`). A bare type applies to every alias of that
    /// type; a dotted form scopes the group to that single instance.
    pub channel: crate::providers::ChannelRef,
    /// Member agents by alias.
    pub agents: Vec<AgentAlias>,
    /// Non-agent members by channel-native username.
    pub external_peers: Vec<PeerUsername>,
    /// Per-group blocklist; subtracts from the resolved peer set.
    pub ignore: Vec<PeerUsername>,
    /// Preferred output modality for all peers in this group.
    /// Defaults to `mirror` (input-driven). Set to `voice` to have the
    /// agent always reply and deliver proactive messages (cron, announces)
    /// as TTS voice notes on channels that support audio output.
    pub output_modality: OutputModality,
    /// When `true`, members of this peer group are authorized to issue
    /// `/model --agent <model>` on the agent this group is bound to.
    /// Default `false` (deny-by-default). The runtime resolves this live
    /// from `Config::peer_groups` at command-dispatch time via
    /// `Config::channel_agent_scope_admins`; no cache, no per-channel
    /// duplicate sender list.
    #[serde(default)]
    pub admin_for_agent_scope: bool,
}

/// Inbound A2A discovery server configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "a2a_server"]
#[serde(default)]
pub struct A2aServerConfig {
    /// Master switch for the inbound A2A surface. Default `false`: no
    /// well-known route, no per-alias cards, no inbound endpoints.
    pub enabled: bool,
    /// Optional advertise-only host override for card endpoint URLs. The
    /// routes always serve on the gateway's own listener; this only changes
    /// the host printed in advertised endpoints when A2A is fronted at a
    /// different address. `None` derives from the gateway host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    /// Optional advertise-only port override, paired with `bind`. `None`
    /// derives from the gateway port. Advertise-only: nothing binds here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Operator-supplied base URL advertised in agent card endpoints.
    pub public_base_url: String,
}

/// A2A section wrapper that leaves room for future sibling configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "a2a"]
#[serde(default)]
pub struct A2aServerSection {
    /// Inbound A2A discovery server (`[a2a.server]`).
    #[nested]
    pub server: A2aServerConfig,
}

/// Per-agent A2A publication and exposed-skill configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent_a2a"]
#[serde(default)]
pub struct AgentA2aConfig {
    /// Publish this alias as a discoverable A2A agent. Default `false`:
    /// the alias is excluded from the discovery catalog and serves no
    /// per-alias card even when the server is enabled.
    pub published: bool,
    /// Filter selecting which resolved skill ids appear on this alias's
    /// card. Empty = no skills advertised. Entries that do not resolve to
    /// a real skill in the alias's bundles are dropped (the bundles are
    /// canonical; this only selects from them).
    pub exposed_skills: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_alias_round_trips_through_serde() {
        // TOML's root must be a table; in real usage AgentAlias lives inside
        // structs. Round-tripping through JSON exercises the same serde path
        // as serialization inside a struct.
        let alias = AgentAlias::new("researcher");
        let json = serde_json::to_string(&alias).unwrap();
        assert_eq!(json, "\"researcher\"");
        let back: AgentAlias = serde_json::from_str(&json).unwrap();
        assert_eq!(alias, back);
    }

    #[test]
    fn access_mode_serializes_snake_case() {
        let cases = [
            (AccessMode::Read, "\"read\""),
            (AccessMode::Write, "\"write\""),
            (AccessMode::ReadWrite, "\"read_write\""),
        ];
        for (mode, expected) in cases {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, expected, "mode={mode:?}");
            let back: AccessMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode);
        }
    }

    #[test]
    fn external_peers_round_trip_as_inline_string_array() {
        let toml_input = r#"
external_peers = ["@user_1", "@user_2"]
"#;
        #[derive(Deserialize)]
        struct Wrapper {
            external_peers: Vec<PeerUsername>,
        }
        let parsed: Wrapper = toml::from_str(toml_input).unwrap();
        assert_eq!(parsed.external_peers.len(), 2);
        assert_eq!(parsed.external_peers[0].as_str(), "@user_1");
        assert_eq!(parsed.external_peers[1].as_str(), "@user_2");
    }

    #[test]
    fn alias_newtypes_are_distinct_at_type_level() {
        // Compile-time: AgentAlias and PeerGroupName don't accidentally
        // assign to each other. The cast through `String` is the only path.
        let agent = AgentAlias::new("alpha");
        let group: PeerGroupName = PeerGroupName::new(agent.as_str());
        assert_eq!(agent.as_str(), group.as_str());
    }

    #[test]
    fn memory_backend_kind_serializes_snake_case() {
        let cases = [
            (MemoryBackendKind::None, "\"none\""),
            (MemoryBackendKind::Sqlite, "\"sqlite\""),
            (MemoryBackendKind::Postgres, "\"postgres\""),
            (MemoryBackendKind::Qdrant, "\"qdrant\""),
            (MemoryBackendKind::Markdown, "\"markdown\""),
            (MemoryBackendKind::Lucid, "\"lucid\""),
        ];
        for (kind, expected) in cases {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, expected, "backend={kind:?}");
            let back: MemoryBackendKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn memory_backend_kind_default_is_sqlite() {
        assert_eq!(MemoryBackendKind::default(), MemoryBackendKind::Sqlite);
    }

    #[test]
    fn agent_workspace_config_round_trips_with_access_map() {
        let toml_input = r#"
unrestricted_filesystem = false
read_memory_from = ["beta"]

[access]
beta = "read"
gamma = "read_write"
"#;
        let parsed: AgentWorkspaceConfig = toml::from_str(toml_input).unwrap();
        assert_eq!(parsed.path, None);
        assert!(!parsed.unrestricted_filesystem);
        assert_eq!(parsed.read_memory_from.len(), 1);
        assert_eq!(parsed.read_memory_from[0], "beta");
        assert_eq!(parsed.access.len(), 2);
        let beta = AgentAlias::new("beta");
        let gamma = AgentAlias::new("gamma");
        assert_eq!(parsed.access.get(&beta), Some(&AccessMode::Read));
        assert_eq!(parsed.access.get(&gamma), Some(&AccessMode::ReadWrite));
    }

    #[test]
    fn agent_workspace_config_default_is_jailed() {
        let cfg = AgentWorkspaceConfig::default();
        assert_eq!(cfg.path, None);
        assert!(cfg.access.is_empty());
        assert!(!cfg.unrestricted_filesystem);
        assert!(cfg.read_memory_from.is_empty());
    }

    #[test]
    fn agent_memory_config_round_trips() {
        let toml_input = r#"backend = "postgres""#;
        let parsed: AgentMemoryConfig = toml::from_str(toml_input).unwrap();
        assert_eq!(parsed.backend, MemoryBackendKind::Postgres);
    }

    #[test]
    fn agent_memory_config_default_is_sqlite() {
        assert_eq!(
            AgentMemoryConfig::default().backend,
            MemoryBackendKind::Sqlite
        );
    }

    #[test]
    fn peer_group_config_round_trips_with_external_peers_and_ignore() {
        let toml_input = r#"
channel = "telegram.prod"
agents = ["alpha", "beta"]
external_peers = ["@user_1", "@user_2"]
ignore = ["@known_spammer"]
"#;
        let parsed: PeerGroupConfig = toml::from_str(toml_input).unwrap();
        assert_eq!(parsed.channel, "telegram.prod");
        assert_eq!(parsed.agents.len(), 2);
        assert_eq!(parsed.agents[0], "alpha");
        assert_eq!(parsed.agents[1], "beta");
        assert_eq!(parsed.external_peers.len(), 2);
        assert_eq!(parsed.external_peers[0].as_str(), "@user_1");
        assert_eq!(parsed.ignore.len(), 1);
        assert_eq!(parsed.ignore[0].as_str(), "@known_spammer");
    }

    #[test]
    fn peer_group_config_default_is_empty() {
        let cfg = PeerGroupConfig::default();
        assert!(cfg.channel.is_empty());
        assert!(cfg.agents.is_empty());
        assert!(cfg.external_peers.is_empty());
        assert!(cfg.ignore.is_empty());
        // Default modality preserves the existing input-driven behavior.
        assert_eq!(cfg.output_modality, OutputModality::Mirror);
    }

    #[test]
    fn output_modality_serializes_snake_case() {
        let cases = [
            (OutputModality::Mirror, "\"mirror\""),
            (OutputModality::Voice, "\"voice\""),
            (OutputModality::Text, "\"text\""),
        ];
        for (modality, expected) in cases {
            let json = serde_json::to_string(&modality).unwrap();
            assert_eq!(json, expected, "modality={modality:?}");
            let back: OutputModality = serde_json::from_str(&json).unwrap();
            assert_eq!(back, modality);
        }
    }

    #[test]
    fn peer_group_output_modality_parses_voice_and_defaults_to_mirror() {
        let with_voice: PeerGroupConfig = toml::from_str(
            r#"
channel = "telegram"
external_peers = ["@alice"]
output_modality = "voice"
"#,
        )
        .unwrap();
        assert_eq!(with_voice.output_modality, OutputModality::Voice);
        assert_eq!(with_voice.external_peers[0].as_str(), "@alice");

        let defaulted: PeerGroupConfig = toml::from_str(r#"channel = "telegram""#).unwrap();
        assert_eq!(defaulted.output_modality, OutputModality::Mirror);
    }

    #[test]
    fn a2a_server_config_default_is_closed_with_no_overrides() {
        let cfg = A2aServerConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.bind.is_none());
        assert!(cfg.port.is_none());
        assert!(cfg.public_base_url.is_empty());
    }

    #[test]
    fn a2a_server_config_round_trips() {
        let toml_input = r#"
enabled = true
bind = "0.0.0.0"
port = 9000
public_base_url = "https://agents.example.com"
"#;
        let parsed: A2aServerConfig = toml::from_str(toml_input).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(parsed.port, Some(9000));
        assert_eq!(parsed.public_base_url, "https://agents.example.com");
    }

    #[test]
    fn agent_a2a_config_default_is_unpublished_with_no_skills() {
        let cfg = AgentA2aConfig::default();
        assert!(!cfg.published);
        assert!(cfg.exposed_skills.is_empty());
    }

    #[test]
    fn agent_a2a_config_round_trips_with_exposed_skills_filter() {
        let toml_input = r#"
published = true
exposed_skills = ["research", "summarize"]
"#;
        let parsed: AgentA2aConfig = toml::from_str(toml_input).unwrap();
        assert!(parsed.published);
        assert_eq!(parsed.exposed_skills.len(), 2);
        assert_eq!(parsed.exposed_skills[0], "research");
        assert_eq!(parsed.exposed_skills[1], "summarize");
    }
}
