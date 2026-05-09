//! Multi-agent runtime types: alias newtypes, access-mode enum, and peer
//! external entries. Backs Issue #6272.
//!
//! These types are the schema-as-law primitives for the multi-agent
//! features landing in v0.8.0:
//!
//! - [`AgentAlias`], [`PeerGroupName`], [`PeerUsername`] are typed string
//!   newtypes that carry their meaning at the type level. They use the
//!   shared `define_provider_ref!` macro defined in [`crate::providers`]
//!   so the on-disk TOML shape stays plain-string while consumers see a
//!   typed value.
//! - [`AccessMode`] is the cross-agent filesystem grant. Read-only,
//!   write-only, or read-write. Default for cross-agent access maps is
//!   "key absent = no grant"; this enum encodes only the granted modes.
//! - [`MemoryBackendKind`] is the per-agent backend selector. Closed set,
//!   no string literals at consumer sites.
//! - [`PeerExternal`] is a single non-agent member of a peer group
//!   (humans, external bots) on the group's channel.
//! - [`AgentWorkspaceConfig`] / [`AgentMemoryConfig`] / [`PeerGroupConfig`]
//!   are the nested config structs the [`crate::schema`] module wires
//!   into [`crate::schema::AliasedAgentConfig`] and the top-level
//!   [`crate::schema::Config`].
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

impl AccessMode {
    /// Whether this mode includes read access.
    #[must_use]
    pub const fn allows_read(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Whether this mode includes write access.
    #[must_use]
    pub const fn allows_write(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// Single non-agent member of a peer group: a human or an external bot reachable
/// at `username` on the group's `channel`. The channel ref lives on the group,
/// so the entry only carries the username.
///
/// Lifted into `[[peer_groups.<name>.external_peers]]` and
/// `[[peer_groups.<name>.ignore]]` arrays.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PeerExternal {
    /// The on-channel username, formatted as the channel kind expects (e.g.
    /// `@beta_bot` for Telegram, `Audacity#0001` for Discord). Validation lives
    /// in `Config::validate()` once the channel kind is known.
    pub username: PeerUsername,
}

/// Per-agent memory backend selector.
///
/// Closed set; the schema is law. Use this enum at every consumer site
/// instead of pattern-matching on the dotted-alias string in the legacy
/// `Config.memory.backend` field. The enum mirrors the storage-instance
/// outer keys under `Config.storage.<kind>.<alias>`: `sqlite`, `postgres`,
/// `qdrant`, `markdown`, `lucid`, plus `none` for the no-storage case.
///
/// Per the multi-agent plan, an agent's backend is locked at agent
/// creation and immutable on subsequent loads. `Config::validate()` is
/// the gate that enforces immutability against the persisted-on-disk
/// state.
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
/// `access` but for the memory layer). Populated entries become the
/// `allowed_agent_ids` set on `AgentScopedMemory<M>` at agent
/// construction; empty means the agent only sees its own memory rows.
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
    /// listed here may appear in this agent's `allowed_agent_ids` set
    /// when `AgentScopedMemory<M>` constructs. Empty = own only.
    pub read_memory_from: Vec<AgentAlias>,
}

/// Per-agent memory backend selection, nested under
/// `[agents.<alias>.memory]`.
///
/// The `backend` field is locked at agent creation and immutable on
/// subsequent loads (`Config::validate()` enforces this against the
/// persisted on-disk state). Cross-backend memory sharing across the
/// per-agent `read_memory_from` allowlist is deferred to v0.8.1; in
/// v0.8.0 the validator rejects allowlist entries that target a
/// different backend than the declaring agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent-memory"]
#[serde(default)]
pub struct AgentMemoryConfig {
    /// The backend kind this agent uses. Defaults to `Sqlite` for new
    /// agents; once an agent has on-disk data the value is locked.
    pub backend: MemoryBackendKind,
}

/// Top-level peer-group block: `[peer_groups.<name>]`.
///
/// Mutual opt-in: two agents become peers only when both appear in the
/// same group's `agents` list. The `channel` field declares which
/// `[channels.<type>.<alias>]` entry the group operates on; the
/// validator at config load enforces that every member's `channels`
/// list includes the group's channel. `external_peers` adds non-agent
/// members (humans, external bots) by username. `ignore` is a per-group
/// blocklist that subtracts from the resolved peer set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "peer-group"]
#[serde(default)]
pub struct PeerGroupConfig {
    /// The channel ref this group operates on (e.g. `"telegram.prod"`).
    /// Must resolve to a configured `[channels.<type>.<alias>]` entry.
    pub channel: crate::providers::ChannelRef,
    /// Member agents by alias. Mutual membership with another agent in
    /// the same group makes them peers.
    pub agents: Vec<AgentAlias>,
    /// Non-agent members on the group's channel.
    pub external_peers: Vec<PeerExternal>,
    /// Group-wide blocklist. Matching usernames are subtracted from the
    /// resolved peer set every member sees.
    pub ignore: Vec<PeerExternal>,
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
    fn access_mode_capability_predicates() {
        assert!(AccessMode::Read.allows_read());
        assert!(!AccessMode::Read.allows_write());
        assert!(!AccessMode::Write.allows_read());
        assert!(AccessMode::Write.allows_write());
        assert!(AccessMode::ReadWrite.allows_read());
        assert!(AccessMode::ReadWrite.allows_write());
    }

    #[test]
    fn peer_external_round_trips() {
        let entry = PeerExternal {
            username: PeerUsername::new("@beta_bot"),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: PeerExternal = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn peer_external_round_trips_through_toml_array() {
        // Real-world shape: peer_groups.<name>.external_peers is an array of
        // tables. Validate the typed shape parses cleanly from that form.
        let toml_input = r#"
[[external_peers]]
username = "@user_1"

[[external_peers]]
username = "@user_2"
"#;
        #[derive(Deserialize)]
        struct Wrapper {
            external_peers: Vec<PeerExternal>,
        }
        let parsed: Wrapper = toml::from_str(toml_input).unwrap();
        assert_eq!(parsed.external_peers.len(), 2);
        assert_eq!(parsed.external_peers[0].username, "@user_1");
        assert_eq!(parsed.external_peers[1].username, "@user_2");
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
        assert_eq!(AgentMemoryConfig::default().backend, MemoryBackendKind::Sqlite);
    }

    #[test]
    fn peer_group_config_round_trips_with_external_peers_and_ignore() {
        let toml_input = r#"
channel = "telegram.prod"
agents = ["alpha", "beta"]

[[external_peers]]
username = "@user_1"

[[external_peers]]
username = "@user_2"

[[ignore]]
username = "@known_spammer"
"#;
        let parsed: PeerGroupConfig = toml::from_str(toml_input).unwrap();
        assert_eq!(parsed.channel, "telegram.prod");
        assert_eq!(parsed.agents.len(), 2);
        assert_eq!(parsed.agents[0], "alpha");
        assert_eq!(parsed.agents[1], "beta");
        assert_eq!(parsed.external_peers.len(), 2);
        assert_eq!(parsed.external_peers[0].username, "@user_1");
        assert_eq!(parsed.ignore.len(), 1);
        assert_eq!(parsed.ignore[0].username, "@known_spammer");
    }

    #[test]
    fn peer_group_config_default_is_empty() {
        let cfg = PeerGroupConfig::default();
        assert!(cfg.channel.is_empty());
        assert!(cfg.agents.is_empty());
        assert!(cfg.external_peers.is_empty());
        assert!(cfg.ignore.is_empty());
    }
}
