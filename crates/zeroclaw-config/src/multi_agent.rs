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
    /// Outbound A2A client (`[a2a.client]`): the agent-to-agent delegation
    /// surface that calls remote A2A peers. Default-closed; opt in via
    /// `[a2a.client] enabled = true`. See the A2ATool RFC.
    #[nested]
    pub client: A2aClientConfig,
}

/// `[a2a.client]` — outbound A2A client (caller role).
///
/// Symmetric counterpart to [`A2aServerConfig`]: the server is the inbound
/// (responder) surface, the client is the outbound (caller) surface that
/// delegates tasks to remote A2A-compliant agents. Default-closed — the
/// four `a2a_*` tools register only when `enabled = true`, so an
/// unconfigured install carries no A2A outbound footprint. Peers are
/// declared statically under `[[a2a.client.peers]]`; DNS auto-discovery is
/// a non-goal (peers are explicitly configured, not auto-discovered).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "a2a_client"]
#[serde(default)]
pub struct A2aClientConfig {
    /// Master switch for the outbound A2A client. Default `false`: no
    /// `a2a_*` tools register, no peer connections are attempted.
    pub enabled: bool,
    /// Per-request timeout (seconds) for JSON-RPC calls to a peer, covering
    /// `message/send` blocking waits. A peer that does not return a
    /// terminal/interrupted task state within this window yields a
    /// `ToolResult::err` with no retry. Default 120s.
    #[serde(default = "default_a2a_client_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Agent Card cache TTL (seconds). `0` disables caching: every
    /// `a2a_discover` / `a2a_send` re-fetches the peer's card. A positive
    /// value caches the parsed card keyed by peer name to avoid repeated
    /// well-known fetches within the window. Default 300s.
    #[serde(default = "default_a2a_client_card_cache_ttl_secs")]
    pub card_cache_ttl_secs: u64,
    /// Allow peers on private/loopback/link-local hosts. Default `false`
    /// (secure-by-default: A2A is an outbound SSRF surface, same posture as
    /// `http_request`). Set `true` for local/intra-net deployments where a
    /// peer lives on `127.0.0.1` or an RFC1918 segment. Reuses the exact
    /// `helpers::domain_guard` policy `http_request` uses — no duplicated
    /// private-host authority.
    #[serde(default)]
    pub allow_private_hosts: bool,
    /// Explicit allowlist of private hosts a peer may target even when
    /// `allow_private_hosts = false`. Entries are domains, hostnames, or IPs
    /// (with `*.suffix` glob support), normalized via
    /// `helpers::domain_guard::normalize_allowed_domains`. Per-host pinning
    /// without loosening the global private-host posture. Default empty.
    #[serde(default)]
    pub allowed_private_hosts: Vec<String>,
    /// Declared remote peers (`[[a2a.client.peers]]`). Empty (with
    /// `enabled = true`) registers the tools but every call fails fast
    /// with "unknown peer" — useful for staging the config before peers
    /// are wired.
    #[nested]
    #[natural_key = "name"]
    pub peers: Vec<A2aClientPeerConfig>,
}

fn default_a2a_client_request_timeout_secs() -> u64 {
    120
}

fn default_a2a_client_card_cache_ttl_secs() -> u64 {
    300
}

impl Default for A2aClientConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            request_timeout_secs: default_a2a_client_request_timeout_secs(),
            card_cache_ttl_secs: default_a2a_client_card_cache_ttl_secs(),
            allow_private_hosts: false,
            allowed_private_hosts: Vec::new(),
            peers: Vec::new(),
        }
    }
}

/// `[[a2a.client.peers]]` — one declared remote A2A peer.
///
/// A peer is an A2A server origin (another ZeroClaw install, or any
/// A2A-compliant agent) identified by its base URL. The optional bearer
/// `token` is resolved from an env var when the value is a `${VAR}`
/// placeholder, reusing the same env-interpolation path as
/// `http_request`'s auth secrets. `tags` are operator metadata surfaced
/// by `a2a_discover` for filtering; they are not protocol-level.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "a2a_client_peer"]
#[serde(default)]
pub struct A2aClientPeerConfig {
    /// Globally unique peer name. Used as the `peer` argument to every
    /// `a2a_*` tool; the agent never types a URL.
    pub name: String,
    /// Base URL of the remote A2A server origin, e.g.
    /// `https://team.example.com`. The well-known card path and the
    /// JSON-RPC task path are derived from this.
    pub base_url: String,
    /// Bearer token for the peer. A `${VAR}` value is resolved from the
    /// environment at call time; a literal value is used as-is. Empty
    /// sends no `Authorization` header (public/anonymous peer).
    pub token: String,
    /// Optional operator tags for `a2a_discover` filtering (e.g.
    /// `["production"]`). Not interpreted by the protocol.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
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
    fn a2a_client_config_default_is_closed_with_sane_timeouts() {
        let cfg = A2aClientConfig::default();
        assert!(!cfg.enabled);
        // Zero timeout would fire immediately on every call; the defaults
        // must be the documented 120s / 300s, not 0.
        assert_eq!(cfg.request_timeout_secs, 120);
        assert_eq!(cfg.card_cache_ttl_secs, 300);
        assert!(cfg.peers.is_empty());
    }

    #[test]
    fn a2a_client_config_round_trips_with_peers() {
        let toml_input = r#"
enabled = true
request_timeout_secs = 60
card_cache_ttl_secs = 0

[[peers]]
name = "team-deploy"
base_url = "https://team.example.com"
token = "${TEAM_DEPLOY_TOKEN}"
tags = ["production"]

[[peers]]
name = "staging"
base_url = "https://staging.example.com"
"#;
        let parsed: A2aClientConfig = toml::from_str(toml_input).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.request_timeout_secs, 60);
        assert_eq!(parsed.card_cache_ttl_secs, 0);
        assert_eq!(parsed.peers.len(), 2);
        assert_eq!(parsed.peers[0].name, "team-deploy");
        assert_eq!(parsed.peers[0].base_url, "https://team.example.com");
        assert_eq!(parsed.peers[0].token, "${TEAM_DEPLOY_TOKEN}");
        assert_eq!(parsed.peers[0].tags, vec!["production".to_string()]);
        // Peer with no tags still parses (skip_serializing_if round-trips
        // through default, not through an absent field).
        assert_eq!(parsed.peers[1].name, "staging");
        assert!(parsed.peers[1].tags.is_empty());
    }

    #[test]
    fn a2a_section_carries_client_sibling_alongside_server() {
        // The `a2a` wrapper must host both `server` and `client` as sibling
        // sub-tables without one moving the other.
        let toml_input = r#"
[server]
enabled = true

[client]
enabled = true

[[client.peers]]
name = "peer-a"
base_url = "https://peer-a.example.com"
"#;
        let parsed: A2aServerSection = toml::from_str(toml_input).unwrap();
        assert!(parsed.server.enabled);
        assert!(parsed.client.enabled);
        assert_eq!(parsed.client.peers.len(), 1);
        assert_eq!(parsed.client.peers[0].name, "peer-a");
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
