//! Multi-agent runtime types: alias newtypes, access-mode enum, peer
//! external entries, and the nested config structs that wire into
//! [`crate::schema::AliasedAgentConfig`] and [`crate::schema::Config`].
//!
//! Cross-agent semantics, peer-group resolution, and SubAgent permission
//! inheritance live in the runtime crate; this module only carries the
//! data shapes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use zeroclaw_macros::Configurable;

crate::define_provider_ref!(AgentAlias, "agents");
crate::define_provider_ref!(PeerGroupName, "peer_groups");
crate::define_provider_ref!(PeerUsername, "channels.peers");

/// Cross-agent filesystem grant.
///
/// Used as the value type in `[agents.<alias>.workspace.access]` maps.
/// A missing entry means no cross-agent access at all (jailed). The enum
/// only encodes the granted modes; absence is the safe default.
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

/// Per-agent memory backend selector.
///
/// Closed set; the schema is law. The enum mirrors the storage-instance
/// outer keys under `Config.storage.<kind>.<alias>`: `sqlite`, `postgres`,
/// `qdrant`, `markdown`, `lucid`, plus `none` for the no-storage case.
///
/// An agent's backend is locked at agent creation and immutable on
/// subsequent loads. `Config::validate()` enforces immutability against
/// the persisted on-disk state.
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

/// Per-agent filesystem and cross-agent access settings, nested under
/// `[agents.<alias>.workspace]`.
///
/// `path = None` means derive the working directory from the install
/// root and agent alias (`<install>/agents/<alias>/workspace/`); set
/// `Some(path)` to put a specific agent's workspace on a different disk
/// or filesystem. The `access` map is the inbound cross-agent filesystem
/// allowlist (key = sibling agent alias, value = read/write/read+write
/// grant); empty means jailed. `unrestricted_filesystem` is the escape
/// hatch for agents that genuinely need to read or write outside any
/// per-agent scope; off by default and audited.
///
/// `read_memory_from` is the cross-agent memory allowlist (parallel to
/// `access` but for the memory layer). The schema validates entries
/// for cross-reference and same-backend invariants at config load.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent-workspace"]
#[serde(default)]
pub struct AgentWorkspaceConfig {
    /// Optional explicit workspace path. `None` = derive from
    /// `<install>/agents/<alias>/workspace/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Cross-agent filesystem allowlist (inbound declaration). Key is
    /// the target sibling agent alias; value is the granted mode. Empty
    /// map = jailed (own workspace only).
    pub access: BTreeMap<AgentAlias, AccessMode>,
    /// Escape hatch: when `true`, the agent can read or write anywhere
    /// the host filesystem permits. Off by default; flipping this on is
    /// auditable.
    pub unrestricted_filesystem: bool,
    /// Cross-agent memory allowlist (inbound declaration). Each alias
    /// listed here is a sibling agent this agent may recall memory
    /// rows from. Empty = own only.
    pub read_memory_from: Vec<AgentAlias>,
}

/// Per-agent memory backend selection, nested under
/// `[agents.<alias>.memory]`.
///
/// The `backend` field is locked at agent creation and immutable on
/// subsequent loads (`Config::validate()` enforces this against the
/// persisted on-disk state). Cross-backend memory sharing across the
/// per-agent `read_memory_from` allowlist is rejected at validation:
/// allowlist entries must point at same-backend siblings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent-memory"]
#[serde(default)]
pub struct AgentMemoryConfig {
    /// The backend kind this agent uses. Defaults to `Sqlite` for new
    /// agents; once an agent has on-disk data the value is locked.
    pub backend: MemoryBackendKind,
}

/// Preferred output modality for a peer group.
///
/// Controls how the agent delivers replies to peers in this group when no
/// stronger per-turn signal is present. `Mirror` (default) preserves the
/// existing input-driven behaviour: voice in → voice out, text in → text out.
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
#[prefix = "peer-group"]
#[serde(default)]
pub struct PeerGroupConfig {
    /// Either a channel type (`"telegram"`) or a dotted channel alias
    /// (`"telegram.work"`). A bare type applies to every alias of that
    /// type; a dotted form scopes the group to that single instance.
    pub channel: String,
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

        // Omitting the field falls back to mirror (current behavior).
        let defaulted: PeerGroupConfig = toml::from_str(r#"channel = "telegram""#).unwrap();
        assert_eq!(defaulted.output_modality, OutputModality::Mirror);
    }
}
