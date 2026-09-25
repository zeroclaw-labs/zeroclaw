// Historical schema typed lenses for migration. Each module is frozen after
// its corresponding version ships; only their `migrate(self) -> ...` methods
// are referenced at runtime by `crate::migration`.
use serde::{Deserialize, Serialize};

/// V3 partial typed lens. V4 removes only fields that this binary no longer
/// supports: inert agent-inline tunables, the superseded summary-model swap,
/// and retired integration/channel spellings. Everything else flows through
/// `passthrough` unchanged.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct V3Config {
    #[serde(default = "default_v3_schema_version")]
    pub schema_version: u32,

    /// V4 drops the inert agent-inline tunable keys (each superseded by the
    /// runtime-profile surface). They deserialized silently into nothing; V4
    /// strips them so a migrated config no longer advertises keys that do
    /// nothing.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub agents: std::collections::HashMap<String, toml::Value>,

    /// V4 drops the deprecated bare `context_compression.summary_model` swap
    /// from every runtime profile; `summary_provider` is the sole surface.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub runtime_profiles: std::collections::HashMap<String, toml::Value>,

    /// Everything else passes through unchanged.
    #[serde(flatten)]
    pub passthrough: toml::Table,
}

fn default_v3_schema_version() -> u32 {
    3
}

/// Inert agent-inline tunable keys removed in V4. Each was superseded by the
/// runtime-profile surface; the agent-inline form deserialized into nothing.
/// Dropped from every `[agents.<alias>]` block during migration.
const V4_INERT_AGENT_KEYS: &[&str] = &[
    "compact_context",
    "max_tool_iterations",
    "max_history_messages",
    "max_context_tokens",
    "memory_recall_limit",
    "parallel_tools",
    "tool_dispatcher",
    "strict_tool_parsing",
];

/// Retired top-level integration spellings. Twitter and Reddit remain live as
/// aliased channels under `[channels]`; only their old singleton roots are
/// unsupported by the current `Config` schema.
const V4_RETIRED_TOP_LEVEL_KEYS: &[&str] = &["twitter", "reddit"];

/// Retired aliased-channel spellings. Notion remains live as the singleton
/// top-level `[notion]` integration, not as `[channels.notion.<alias>]`.
const V4_RETIRED_CHANNEL_KEYS: &[&str] = &["notion"];

impl V3Config {
    /// Returns a V4-shaped `toml::Value`. The caller deserializes it into
    /// `Config` — that round-trip is the gate that catches any structural
    /// mismatch.
    pub fn migrate(self) -> anyhow::Result<toml::Value> {
        let V3Config {
            schema_version: _,
            agents,
            runtime_profiles,
            mut passthrough,
        } = self;

        let new_agents = drop_inert_agent_keys(agents);
        if !new_agents.is_empty() {
            passthrough.insert("agents".to_string(), toml::Value::Table(new_agents));
        }

        let new_profiles = drop_summary_model_swap(runtime_profiles);
        if !new_profiles.is_empty() {
            passthrough.insert(
                "runtime_profiles".to_string(),
                toml::Value::Table(new_profiles),
            );
        }

        drop_retired_top_level_keys(&mut passthrough);
        drop_retired_channel_keys(&mut passthrough);
        drop_retired_peer_groups(&mut passthrough);

        passthrough.insert("schema_version".to_string(), toml::Value::Integer(4));

        Ok(toml::Value::Table(passthrough))
    }
}

fn drop_inert_agent_keys(agents: std::collections::HashMap<String, toml::Value>) -> toml::Table {
    let mut out = toml::Table::new();
    for (alias, value) in agents {
        let cleaned = match value {
            toml::Value::Table(mut agent_table) => {
                let mut dropped = Vec::new();
                for key in V4_INERT_AGENT_KEYS {
                    if agent_table.remove(*key).is_some() {
                        dropped.push(*key);
                    }
                }
                if !dropped.is_empty() {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "[agents.{alias}] inert tunable keys dropped: {dropped:?} (runtime profiles are authoritative)"
                        )
                    );
                }
                prune_retired_channel_refs(&alias, &mut agent_table);
                toml::Value::Table(agent_table)
            }
            other => other,
        };
        out.insert(alias, cleaned);
    }
    out
}

fn prune_retired_channel_refs(alias: &str, agent_table: &mut toml::Table) {
    let Some(toml::Value::Array(channels)) = agent_table.get_mut("channels") else {
        return;
    };
    let mut removed = Vec::new();
    channels.retain(|entry| {
        let Some(reference) = entry.as_str() else {
            return true;
        };
        let channel_type = reference.split('.').next().unwrap_or(reference);
        let retired = V4_RETIRED_CHANNEL_KEYS.contains(&channel_type);
        if retired {
            removed.push(reference.to_string());
        }
        !retired
    });
    if !removed.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("[agents.{alias}.channels] dropped refs to retired channels: {removed:?}")
        );
    }
}

fn drop_summary_model_swap(
    runtime_profiles: std::collections::HashMap<String, toml::Value>,
) -> toml::Table {
    let mut out = toml::Table::new();
    for (alias, value) in runtime_profiles {
        let cleaned = match value {
            toml::Value::Table(mut profile) => {
                if let Some(toml::Value::Table(cc)) = profile.get_mut("context_compression")
                    && cc.remove("summary_model").is_some()
                {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "[runtime_profiles.{alias}.context_compression] summary_model dropped (use summary_provider)"
                        )
                    );
                }
                toml::Value::Table(profile)
            }
            other => other,
        };
        out.insert(alias, cleaned);
    }
    out
}

fn drop_retired_top_level_keys(passthrough: &mut toml::Table) {
    let mut dropped = Vec::new();
    for key in V4_RETIRED_TOP_LEVEL_KEYS {
        if passthrough.remove(*key).is_some() {
            dropped.push(*key);
        }
    }
    if !dropped.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("retired top-level integration sections dropped: {dropped:?}")
        );
    }
}

fn drop_retired_channel_keys(passthrough: &mut toml::Table) {
    let Some(toml::Value::Table(channels)) = passthrough.get_mut("channels") else {
        return;
    };
    let mut dropped = Vec::new();
    for key in V4_RETIRED_CHANNEL_KEYS {
        if channels.remove(*key).is_some() {
            dropped.push(*key);
        }
    }
    if !dropped.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("retired channel sections dropped: {dropped:?}")
        );
    }
}

fn drop_retired_peer_groups(passthrough: &mut toml::Table) {
    let Some(toml::Value::Table(peer_groups)) = passthrough.get_mut("peer_groups") else {
        return;
    };
    let removed: Vec<String> = peer_groups
        .iter()
        .filter_map(|(name, group)| {
            let channel_type = group
                .as_table()
                .and_then(|table| table.get("channel"))
                .and_then(toml::Value::as_str)
                .map(|reference| reference.split('.').next().unwrap_or(reference));
            channel_type
                .is_some_and(|kind| V4_RETIRED_CHANNEL_KEYS.contains(&kind))
                .then(|| name.clone())
        })
        .collect();
    for name in &removed {
        peer_groups.remove(name);
    }
    if !removed.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("peer groups bound to retired channels dropped: {removed:?}")
        );
    }
}
