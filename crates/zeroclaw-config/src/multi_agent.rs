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
    /// Retired PostgreSQL backend. Still parses so an old config loads (with
    /// the `memory_backend_removed` warning); selecting it fails memory
    /// construction explicitly.
    Postgres,
    /// Retired Qdrant backend. Same contract as [`Self::Postgres`].
    Qdrant,
    /// Markdown files in the agent's workspace
    /// (`crates/zeroclaw-memory/src/markdown.rs`).
    Markdown,
    /// Hybrid local SQLite + external Lucid CLI
    /// (`crates/zeroclaw-memory/src/lucid.rs`).
    Lucid,
    /// Embedded Tachi portable kernel (`crates/zeroclaw-memory/src/tachi.rs`).
    Tachi,
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

fn normalize_telegram_identity(value: &str) -> String {
    value.trim().trim_start_matches('@').to_string()
}

/// Per-channel-type identity normalizer. The operator-bind op is otherwise
/// identical across the pairing-capable channels; the only variance is how a
/// raw identity is canonicalized before it is stored in the allowlist.
pub type ChannelIdentityNormalizer = fn(&str) -> String;

/// Resolve the identity normalizer for a pairing-capable channel type, or
/// `None` for a type with no operator-bind surface. `None` is the closed-set
/// gate: only `telegram` / `wechat` / `line` can be bound this way.
#[must_use]
pub fn channel_identity_normalizer(channel_type: &str) -> Option<ChannelIdentityNormalizer> {
    match channel_type {
        "telegram" => Some(normalize_telegram_identity),
        _ => None,
    }
}

/// Whether a `[channels.<type>.<alias>]` section exists. Rust has no
/// reflection over the typed channel maps, so this stays an explicit per-type
/// match; only this arm grows when a new pairing channel lands.
#[must_use]
pub fn channel_alias_configured(
    config: &crate::schema::Config,
    channel_type: &str,
    alias: &str,
) -> bool {
    match channel_type {
        "telegram" => config.channels.telegram.contains_key(alias),
        _ => false,
    }
}

/// Add `identity` to the peer group bound to `<type>.<alias>` in-place.
///
/// Returns `Ok(true)` when the identity was newly added, `Ok(false)` when it
/// was already present. Pure config mutation — no disk write, no daemon
/// restart — so it is the single core shared by the CLI
/// (`bind_telegram_identity`) and the gateway bind endpoint. The `channel`
/// field is the dotted `<type>.<alias>` ref so authorization stays scoped to
/// the bound alias; a bare type would broaden the peer across every alias of
/// that type.
pub fn bind_channel_identity_into(
    config: &mut crate::schema::Config,
    channel_type: &str,
    alias: &str,
    identity: &str,
) -> anyhow::Result<bool> {
    let Some(normalize) = channel_identity_normalizer(channel_type) else {
        anyhow::bail!(
            "Channel type `{channel_type}` does not support identity binding \
             (supported: telegram)."
        );
    };

    let normalized = normalize(identity);
    if normalized.is_empty() {
        anyhow::bail!("{channel_type} identity cannot be empty");
    }

    // The alias must name an existing `[channels.<type>.<alias>]` section.
    // Binding into a phantom alias would mint a peer group the runtime never
    // reads (it resolves authorization per the alias the channel actually
    // runs under), so fail loudly instead of silently authorizing nobody.
    if !channel_alias_configured(config, channel_type, alias) {
        anyhow::bail!(
            "{channel_type} channel alias `{alias}` is not configured. Run \
             `zeroclaw config set channels.{channel_type}.{alias}.bot_token <token>` \
             (see docs/book/src/channels/overview.md for the full field list)."
        );
    }

    let group_name = format!("{channel_type}_{alias}");
    let channel_ref = format!("{channel_type}.{alias}");
    let group = config
        .peer_groups
        .entry(group_name)
        .or_insert_with(|| PeerGroupConfig {
            channel: crate::providers::ChannelRef::new(channel_ref),
            ..PeerGroupConfig::default()
        });

    if group
        .external_peers
        .iter()
        .any(|p| normalize(p.as_str()) == normalized)
    {
        return Ok(false);
    }

    group.external_peers.push(PeerUsername::new(normalized));
    Ok(true)
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
            (MemoryBackendKind::Tachi, "\"tachi\""),
        ];
        for (kind, expected) in cases {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, expected, "backend={kind:?}");
            let back: MemoryBackendKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
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
        let toml_input = r#"backend = "markdown""#;
        let parsed: AgentMemoryConfig = toml::from_str(toml_input).unwrap();
        assert_eq!(parsed.backend, MemoryBackendKind::Markdown);
        // The retired backends still parse so old configs keep loading.
        let retired: AgentMemoryConfig = toml::from_str(r#"backend = "postgres""#).unwrap();
        assert_eq!(retired.backend, MemoryBackendKind::Postgres);
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

    fn config_with_telegram_alias(alias: &str) -> crate::schema::Config {
        let mut config = crate::schema::Config::default();
        config.channels.telegram.insert(
            alias.to_string(),
            crate::schema::TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn bind_telegram_into_non_default_alias_is_resolvable() {
        let mut config = config_with_telegram_alias("alerts");
        let newly =
            bind_channel_identity_into(&mut config, "telegram", "alerts", "123456789").unwrap();
        assert!(newly, "first bind should report newly added");
        assert!(
            config
                .channel_external_peers("telegram", "alerts")
                .contains(&"123456789".to_string()),
            "identity bound to `alerts` must resolve for the `alerts` channel"
        );
        assert!(
            config
                .channel_external_peers("telegram", "other")
                .is_empty(),
            "binding must stay scoped to its alias"
        );
    }

    #[test]
    fn bind_telegram_into_default_alias_unchanged() {
        let mut config = config_with_telegram_alias("default");
        bind_channel_identity_into(&mut config, "telegram", "default", "@zeroclaw_user").unwrap();
        let group = config
            .peer_groups
            .get("telegram_default")
            .expect("default bind must use the telegram_default group");
        assert_eq!(group.channel.as_str(), "telegram.default");
        assert!(
            config
                .channel_external_peers("telegram", "default")
                .contains(&"zeroclaw_user".to_string())
        );
    }

    #[test]
    fn bind_telegram_into_is_idempotent() {
        let mut config = config_with_telegram_alias("alerts");
        assert!(bind_channel_identity_into(&mut config, "telegram", "alerts", "123").unwrap());
        assert!(
            !bind_channel_identity_into(&mut config, "telegram", "alerts", "123").unwrap(),
            "second bind of same identity should report already present"
        );
        assert_eq!(
            config.channel_external_peers("telegram", "alerts").len(),
            1,
            "duplicate bind must not add a second peer entry"
        );
    }

    #[test]
    fn bind_telegram_into_unconfigured_alias_bails() {
        let mut config = config_with_telegram_alias("default");
        let err = bind_channel_identity_into(&mut config, "telegram", "typoalias", "123")
            .expect_err("unconfigured alias must bail");
        assert!(
            err.to_string().contains("typoalias"),
            "error should name the bad alias, got: {err}"
        );
        assert!(
            !config.peer_groups.contains_key("telegram_typoalias"),
            "failed bind must not create a phantom peer group"
        );
    }

    #[test]
    fn bind_channel_into_uses_scoped_dotted_channel_ref() {
        let mut config = config_with_telegram_alias("alerts");
        assert!(bind_channel_identity_into(&mut config, "telegram", "alerts", "@user").unwrap());
        let group = config
            .peer_groups
            .get("telegram_alerts")
            .expect("generic bind must use the telegram_alerts group");
        assert_eq!(
            group.channel.as_str(),
            "telegram.alerts",
            "channel ref must stay dotted/alias-scoped, never bare `telegram`"
        );
        assert!(
            config
                .channel_external_peers("telegram", "alerts")
                .contains(&"user".to_string())
        );
        assert!(
            config
                .channel_external_peers("telegram", "other")
                .is_empty()
        );
    }

    #[test]
    fn bind_channel_into_rejects_unsupported_type() {
        let mut config = crate::schema::Config::default();
        let err = bind_channel_identity_into(&mut config, "discord", "default", "123")
            .expect_err("unsupported type must bail");
        assert!(
            err.to_string()
                .contains("does not support identity binding"),
            "error should explain the type is unsupported, got: {err}"
        );
        assert!(
            channel_identity_normalizer("discord").is_none(),
            "discord must not be in the bind closed-set"
        );
        assert!(channel_identity_normalizer("telegram").is_some());
        assert!(channel_identity_normalizer("wechat").is_none());
        assert!(channel_identity_normalizer("line").is_none());
    }
}
