use serde::{Deserialize, Serialize};

/// V3 partial typed lens. V4 is a pure key-drop: no restructuring, no field
/// moves. Everything not explicitly named flows through `passthrough`
/// unchanged. The named slots are the tables V4 reaches into to strip dead
/// nested keys the code already ignores.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct V3Config {
    #[serde(default = "default_v3_schema_version")]
    pub schema_version: u32,

    /// V4 drops the deprecated `prompt_injection_mode` field (skills render
    /// is compact-only; the `full` mode was already inert).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<toml::Value>,

    /// V4 drops the inert agent-inline tunable keys (superseded by runtime
    /// profiles, #6877). They deserialized silently into nothing; V4 strips
    /// them so a migrated config no longer advertises keys that do nothing.
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
/// runtime-profile surface (#6877); the agent-inline form deserialized into
/// nothing. Dropped from every `[agents.<alias>]` block during migration.
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

impl V3Config {
    /// Returns a V4-shaped `toml::Value`. The caller deserializes it into
    /// `Config` — that round-trip is the gate that catches any structural
    /// mismatch.
    pub fn migrate(self) -> anyhow::Result<toml::Value> {
        let V3Config {
            schema_version: _,
            skills,
            agents,
            runtime_profiles,
            mut passthrough,
        } = self;

        if let Some(new_skills) = drop_skills_prompt_injection_mode(skills) {
            passthrough.insert("skills".to_string(), new_skills);
        }

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

        passthrough.insert("schema_version".to_string(), toml::Value::Integer(4));

        Ok(toml::Value::Table(passthrough))
    }
}

fn drop_skills_prompt_injection_mode(skills: Option<toml::Value>) -> Option<toml::Value> {
    let toml::Value::Table(mut table) = skills? else {
        return None;
    };
    if table.remove("prompt_injection_mode").is_some() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "[skills] prompt_injection_mode dropped (compact-only render in V4)"
        );
    }
    Some(toml::Value::Table(table))
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
                toml::Value::Table(agent_table)
            }
            other => other,
        };
        out.insert(alias, cleaned);
    }
    out
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
