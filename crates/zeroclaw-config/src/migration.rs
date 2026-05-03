//! Forward-only config schema migration.
//!
//! Old config layouts are typed structs. Migration deserializes into the legacy
//! struct, moves field values into the new layout, and returns a clean `Config`.
//!
//! The on-disk file is never rewritten by migration.
//!
//! ## When to bump the schema version
//!
//! Only when props are **renamed, moved, or removed**. New props with `#[serde(default)]`
//! don't need a bump.
//!
//! ## How to add a new migration step
//!
//! 1. Bump [`CURRENT_SCHEMA_VERSION`].
//! 2. Add structural TOML-level rewrites to the appropriate `v{N}_to_v{M}_table()`
//!    function (shape changes that must happen before serde deserialization).
//!    `prepare_table()` dispatches to these based on `schema_version`.
//! 3. Add a `fn vN_to_vM(config: &mut Config)` for any in-memory work that can
//!    only run after deserialization, and gate it in `into_config()` on `from < M`.
//!    V1 fields are the exception — they live on `V1Compat` and are handled by
//!    `migrate_providers()`, gated on `from < 2 || has_legacy_fields()`.
//! 4. Add a test in `tests/component/config_migration.rs`:
//!    - Deserialize a TOML string with the old layout.
//!    - Assert the migrated `Config` has values in the new locations.
//!    - Assert the old locations are empty/cleared.
//! 5. Verify with `cargo test --test component -- config_migration`.
//!
//! ## User-facing migration command
//!
//! `zeroclaw config migrate` rewrites the on-disk `config.toml` to the current
//! schema version using `toml_edit` to preserve comments and formatting.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use toml_edit::DocumentMut;

use super::schema::ModelProviderConfig;

pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Top-level keys from V1 that are consumed by V1Compat during migration.
/// Used by the unknown-key detector to suppress false "unknown key" warnings.
pub const V1_LEGACY_KEYS: &[&str] = &[
    "api_key",
    "api_url",
    "api_path",
    "default_provider",
    "model_provider",
    "default_model",
    "model",
    "default_temperature",
    "provider_timeout_secs",
    "provider_max_tokens",
    "extra_headers",
    "model_providers",
    "model_routes",
    "embedding_routes",
    "channels_config",
];

/// Wraps the current Config with extra fields from V1 that no longer exist on Config.
/// `#[serde(flatten)]` lets Config consume its known fields; the old fields are
/// captured here.
#[derive(Deserialize)]
pub struct V1Compat {
    #[serde(flatten)]
    pub config: super::schema::Config,

    // ── Old top-level provider fields (removed in V2) ──
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_url: Option<String>,
    #[serde(default)]
    api_path: Option<String>,
    #[serde(default, alias = "model_provider")]
    default_provider: Option<String>,
    #[serde(default, alias = "model")]
    default_model: Option<String>,
    #[serde(default)]
    model_providers: HashMap<String, ModelProviderConfig>,
    #[serde(default)]
    default_temperature: Option<f64>,
    #[serde(default)]
    provider_timeout_secs: Option<u64>,
    #[serde(default)]
    provider_max_tokens: Option<u32>,
    #[serde(default)]
    extra_headers: Option<HashMap<String, String>>,
    #[serde(default)]
    model_routes: Vec<super::schema::ModelRouteConfig>,
    #[serde(default)]
    embedding_routes: Vec<super::schema::EmbeddingRouteConfig>,
}

impl V1Compat {
    /// Consume self, running each versioned migration step in order.
    pub fn into_config(mut self) -> super::schema::Config {
        let from = self.config.schema_version;
        let needs_migration = from < CURRENT_SCHEMA_VERSION || self.has_legacy_fields();

        if !needs_migration {
            return self.config;
        }

        if from < 2 || self.has_legacy_fields() {
            self.v1_to_v2();
        }
        if from < 3 {
            v2_to_v3(&mut self.config);
        }

        self.config.schema_version = CURRENT_SCHEMA_VERSION;

        tracing::info!(
            from = from,
            to = CURRENT_SCHEMA_VERSION,
            "Config schema migrated in-memory from version {from} to {CURRENT_SCHEMA_VERSION}. \
             Run `zeroclaw config migrate` to update the file on disk.",
        );

        self.config
    }

    /// Parse a V1 fixture TOML string into V1Compat, running `prepare_table`
    /// first so field shapes are normalised before serde deserialization —
    /// same path as a real config load.
    fn from_v1_fixture(raw: &str) -> Result<Self, String> {
        let mut table: toml::Table =
            toml::from_str(raw).map_err(|e| format!("failed to parse v1 fixture table: {e}"))?;
        prepare_table(&mut table);
        let prepared = toml::to_string(&table)
            .map_err(|e| format!("failed to re-serialize v1 fixture: {e}"))?;
        toml::from_str(&prepared).map_err(|e| format!("failed to deserialize v1 fixture: {e}"))
    }

    /// Serialize into V2 TOML shape.
    ///
    /// Starts from the full serialized config (so all sections are present), then
    /// downgrades only the V3-specific shapes back to V2:
    /// - `providers.fallback`: array → bare string (first entry, type portion only)
    /// - `providers.models.<type>`: nested alias map → flat table (take "default" alias)
    /// - `channels.<type>`: aliased sub-tables → flat table (take "default" alias)
    /// - V3-only top-level sections (risk_profiles, runtime_profiles, agents, etc.)
    ///   are stripped — they have no V2 equivalent.
    fn snapshot_v2(&self) -> toml::Table {
        let raw = toml::to_string(&self.config).unwrap_or_default();
        let mut config_table: toml::Table = toml::from_str(&raw).unwrap_or_default();
        config_table.insert("schema_version".into(), toml::Value::Integer(2));

        if let Some(toml::Value::Table(providers)) = config_table.get_mut("providers") {
            // Strip any lingering fallback key from V2 configs during downgrade.
            providers.remove("fallback");

            // Downgrade providers.models: nested alias map → flat (take "default" alias).
            if let Some(toml::Value::Table(models)) = providers.get_mut("models") {
                let type_keys: Vec<String> = models.keys().cloned().collect();
                for type_key in type_keys {
                    if let Some(toml::Value::Table(alias_map)) = models.get(&type_key).cloned() {
                        // If already flat (no sub-tables), leave as-is.
                        if alias_map
                            .values()
                            .any(|v| !matches!(v, toml::Value::Table(_)))
                        {
                            continue;
                        }
                        if let Some(toml::Value::Table(default_entry)) =
                            alias_map.get("default").cloned()
                        {
                            models.insert(type_key, toml::Value::Table(default_entry));
                        }
                    }
                }
            }
        }

        // Downgrade channels: aliased sub-tables → flat (take "default" alias).
        if let Some(toml::Value::Table(channels)) = config_table.get_mut("channels") {
            let ch_keys: Vec<String> = channels.keys().cloned().collect();
            for ch_type in ch_keys {
                if let Some(toml::Value::Table(alias_map)) = channels.get(&ch_type).cloned() {
                    if alias_map
                        .values()
                        .any(|v| !matches!(v, toml::Value::Table(_)))
                    {
                        // Already flat.
                        continue;
                    }
                    if let Some(toml::Value::Table(default_entry)) =
                        alias_map.get("default").cloned()
                    {
                        channels.insert(ch_type, toml::Value::Table(default_entry));
                    }
                }
            }
            // Keep non-channel scalar fields (e.g. ack_reactions, cli) by leaving them.
        }

        // Strip V3-only top-level sections that do not exist in V2.
        for key in &[
            "risk_profiles",
            "runtime_profiles",
            "memory_namespaces",
            "skill_bundles",
            "mcp_bundles",
            "knowledge_bundles",
        ] {
            config_table.remove(*key);
        }

        // Agents in V2 used inline provider/model/temperature — strip model_provider.
        if let Some(toml::Value::Table(agents)) = config_table.get_mut("agents") {
            let agent_keys: Vec<String> = agents.keys().cloned().collect();
            for key in agent_keys {
                if let Some(toml::Value::Table(at)) = agents.get_mut(&key) {
                    at.remove("model_provider");
                    at.remove("model_provider_fallback");
                }
            }
        }

        config_table
    }

    fn snapshot_current(&self) -> toml::Table {
        let mut t: toml::Table =
            toml::from_str(&toml::to_string(&self.config).unwrap_or_default()).unwrap_or_default();
        // The Config struct still carries V2 fields that prepare_table strips when
        // loading a V3 file. Strip them here so the canonical V3 fixture does not
        // re-emit sections that migration removes (autonomy, agent, security
        // subsections, swarms). Snapshot must match what a round-tripped load produces.
        t.remove("autonomy");
        t.remove("agent");
        t.remove("swarms");
        if let Some(toml::Value::Table(security)) = t.get_mut("security") {
            security.remove("sandbox");
            security.remove("resources");
        }
        t
    }

    fn has_legacy_fields(&self) -> bool {
        self.api_key.is_some()
            || self.api_url.is_some()
            || self.api_path.is_some()
            || self.default_provider.is_some()
            || self.default_model.is_some()
            || !self.model_providers.is_empty()
            || self.default_temperature.is_some()
            || self.provider_timeout_secs.is_some()
            || self.provider_max_tokens.is_some()
            || self.extra_headers.as_ref().is_some_and(|h| !h.is_empty())
            || !self.model_routes.is_empty()
            || !self.embedding_routes.is_empty()
    }

    fn v1_to_v2(&mut self) {
        // First, move old model_providers entries into providers.models.
        // V1 top-level entries use bare type keys ("anthropic"); insert under "default" alias.
        for (key, profile) in std::mem::take(&mut self.model_providers) {
            self.config
                .providers
                .models
                .entry(key)
                .or_default()
                .entry("default".to_string())
                .or_insert(profile);
        }

        // Only create a fallback scaffolding entry if there are V1 top-level fields
        // to migrate into it. If none exist, there is nothing to write and creating
        // an empty entry would produce an invalid config.
        let has_v1_fields = self.api_key.is_some()
            || self.api_url.is_some()
            || self.api_path.is_some()
            || self.default_model.is_some()
            || self.default_temperature.is_some()
            || self.provider_timeout_secs.is_some()
            || self.provider_max_tokens.is_some()
            || self.extra_headers.as_ref().is_some_and(|h| !h.is_empty());

        if !has_v1_fields {
            if let Some(provider) = self.default_provider.take()
                && self.config.providers.models.is_empty()
            {
                self.config
                    .providers
                    .models
                    .entry(provider.clone())
                    .or_default()
                    .entry("default".to_string())
                    .or_default();
            }
            return;
        }

        let fallback = self
            .default_provider
            .take()
            .or_else(|| {
                self.config
                    .providers
                    .first_provider_type()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "default".to_string());

        // Fill gaps in the fallback entry from top-level V1 fields.
        // fallback is a bare type name (e.g. "anthropic"); map to "default" alias in nested map.
        let entry = self
            .config
            .providers
            .models
            .entry(fallback.clone())
            .or_default()
            .entry("default".to_string())
            .or_default();

        if entry.api_key.is_none() {
            entry.api_key = self.api_key.take();
        }
        if entry.base_url.is_none() {
            entry.base_url = self.api_url.take();
        }
        if entry.api_path.is_none() {
            entry.api_path = self.api_path.take();
        }
        if entry.model.is_none() {
            entry.model = self.default_model.take();
        }
        if entry.temperature.is_none() {
            entry.temperature = self.default_temperature.take();
        }
        if entry.timeout_secs.is_none() {
            entry.timeout_secs = self.provider_timeout_secs.take();
        }
        if entry.max_tokens.is_none() {
            entry.max_tokens = self.provider_max_tokens.take();
        }
        if entry.extra_headers.is_empty()
            && let Some(headers) = self.extra_headers.take()
        {
            entry.extra_headers = headers;
        }

        // Move routing rules into providers.
        if self.config.providers.model_routes.is_empty() && !self.model_routes.is_empty() {
            self.config.providers.model_routes = std::mem::take(&mut self.model_routes);
        }
        if self.config.providers.embedding_routes.is_empty() && !self.embedding_routes.is_empty() {
            self.config.providers.embedding_routes = std::mem::take(&mut self.embedding_routes);
        }

        // Populate providers.fallback so v2_to_v3() can normalize it to a dotted alias ref.
        if !self.config.providers.fallback.contains(&fallback) {
            self.config.providers.fallback.push(fallback);
        }
    }
}

/// Move a scalar string field into a `Vec<String>` field on the same TOML table.
///
/// Removes `old_key` from `section`. If the value is non-empty and not `"*"`,
/// appends it to `new_key` (creating the array if absent), deduplicating in
/// place. `"*"` wildcards and empty strings are silently dropped — they had
/// no meaningful plural equivalent.
///
/// This is the canonical typed transformation path for scalar→vec field
/// renames in the migration system. All compound-field shape changes in
/// `prepare_table` should call this helper rather than inlining the pattern.
pub(crate) fn scalar_to_vec(section: &mut toml::Table, old_key: &str, new_key: &str) {
    if let Some(toml::Value::String(val)) = section.remove(old_key)
        && !val.is_empty()
        && val != "*"
    {
        let arr = section
            .entry(new_key.to_string())
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        if let toml::Value::Array(vec) = arr
            && !vec.iter().any(|v| v.as_str() == Some(val.as_str()))
        {
            vec.push(toml::Value::String(val));
        }
    }
}

/// All channel type keys recognised in V2 `[channels]` that the aliasing
/// migration wraps into `[channels.<type>.default]`.
const CHANNEL_TYPES: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "mattermost",
    "webhook",
    "imessage",
    "matrix",
    "signal",
    "whatsapp",
    "linq",
    "wati",
    "nextcloud_talk",
    "email",
    "gmail_push",
    "irc",
    "lark",
    "line",
    "feishu",
    "dingtalk",
    "wecom",
    "wechat",
    "qq",
    "twitter",
    "mochat",
    "nostr",
    "clawdtalk",
    "reddit",
    "bluesky",
    "voice_call",
    "voice_wake",
    "voice_duplex",
    "mqtt",
];

/// Return true if this TOML table is a V2 flat channel config (has at least one
/// non-table leaf value at the top level). V3 alias maps contain only sub-tables.
fn is_flat_channel_config(t: &toml::Table) -> bool {
    t.values().any(|v| !matches!(v, toml::Value::Table(_)))
}

/// Return true if this TOML table is a V2 flat model-provider config (has at least one
/// non-table leaf value). V3 alias maps contain only sub-tables (one per alias).
fn is_flat_provider_config(t: &toml::Table) -> bool {
    t.values().any(|v| !matches!(v, toml::Value::Table(_)))
}

/// V2 → V3 in-memory migration. TOML-level transforms run in `prepare_table`
/// before deserialization; add post-deserialization work here as needed.
fn v2_to_v3(config: &mut super::schema::Config) {
    // Normalize providers.fallback entries from bare type names ("myprovider")
    // to dotted type.alias refs ("myprovider.default") as required by V3.
    for entry in &mut config.providers.fallback {
        if !entry.contains('.') {
            *entry = format!("{entry}.default");
        }
    }
}

/// Pre-deserialization table migration dispatcher.
///
/// Reads `schema_version` from the raw table and calls only the transforms
/// needed to bring it up to the current schema. Each version function is
/// responsible for all TOML-level shape changes between those two versions.
/// Called before deserialization into `V1Compat`.
pub fn prepare_table(table: &mut toml::Table) {
    let version = table
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .unwrap_or(0) as u32;

    if version < 2 {
        v1_to_v2_table(table);
    }
    if version < 3 {
        v2_to_v3_table(table);
    }
}

/// V1 → V2 TOML-level transforms: scalar field renames that must happen before
/// serde deserialization can see the V2 field names.
fn v1_to_v2_table(table: &mut toml::Table) {
    // channels_config.matrix.room_id → channels_config.matrix.allowed_rooms
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(matrix)) = channels.get_mut("matrix")
        {
            scalar_to_vec(matrix, "room_id", "allowed_rooms");
        }
    }

    // channels.slack.channel_id → channels.slack.channel_ids
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(slack)) = channels.get_mut("slack")
        {
            scalar_to_vec(slack, "channel_id", "channel_ids");
        }
    }

    // channels.mattermost.channel_id → channels.mattermost.channel_ids
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(mattermost)) = channels.get_mut("mattermost")
        {
            scalar_to_vec(mattermost, "channel_id", "channel_ids");
        }
    }

    // channels.discord.guild_id → channels.discord.guild_ids
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(discord)) = channels.get_mut("discord")
        {
            scalar_to_vec(discord, "guild_id", "guild_ids");
        }
    }

    // channels.signal.group_id → channels.signal.{group_ids, dm_only}
    // The legacy field overloaded a sentinel "dm" for DMs-only mode; split that
    // into a typed bool.
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(signal)) = channels.get_mut("signal")
            && let Some(toml::Value::String(group_id)) = signal.remove("group_id")
            && !group_id.is_empty()
        {
            if group_id == "dm" {
                signal.insert("dm_only".to_string(), toml::Value::Boolean(true));
            } else {
                let ids = signal
                    .entry("group_ids")
                    .or_insert_with(|| toml::Value::Array(Vec::new()));
                if let toml::Value::Array(arr) = ids {
                    let already_present = arr.iter().any(|v| v.as_str() == Some(group_id.as_str()));
                    if !already_present {
                        arr.push(toml::Value::String(group_id));
                    }
                }
            }
        }
    }

    // channels.reddit.subreddit → channels.reddit.subreddits
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(reddit)) = channels.get_mut("reddit")
        {
            scalar_to_vec(reddit, "subreddit", "subreddits");
        }
    }

    // Fold [channels.discord-history] into [channels.discord].
    // discord-history was a separate channel type that archived ALL messages.
    // V3 merges it into discord with `archive = true`.
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key) {
            let dh = channels
                .remove("discord-history")
                .or_else(|| channels.remove("discord_history"));
            if let Some(toml::Value::Table(mut dh_table)) = dh {
                scalar_to_vec(&mut dh_table, "guild_id", "guild_ids");
                dh_table.remove("store_dms");
                dh_table.remove("respond_to_dms");

                if let Some(toml::Value::Table(discord)) = channels.get_mut("discord") {
                    let dh_token = dh_table
                        .get("bot_token")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let dc_token = discord
                        .get("bot_token")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !dh_token.is_empty() && dh_token != dc_token {
                        tracing::warn!(
                            "v1→v2 migration: [channels.discord-history] has a different \
                             bot_token than [channels.discord]. Discarding discord-history \
                             config; re-configure archive manually under [channels.discord]."
                        );
                    } else {
                        discord.insert("archive".to_string(), toml::Value::Boolean(true));
                        if let Some(dh_ids) = dh_table.remove("channel_ids")
                            && discord.get("channel_ids").is_none()
                        {
                            discord.insert("channel_ids".to_string(), dh_ids);
                        }
                    }
                } else {
                    dh_table.insert("archive".to_string(), toml::Value::Boolean(true));
                    channels.insert("discord".to_string(), toml::Value::Table(dh_table));
                }
            }
        }
    }
}

/// V2 → V3 TOML-level transforms: aliasing wraps, section synthesis, field
/// removals, and renames that must happen before deserialization into V1Compat.
fn v2_to_v3_table(table: &mut toml::Table) {
    // Read non_cli_excluded_tools before the wrap so we can propagate it to
    // each channel alias's excluded_tools field. The flat autonomy field does
    // not survive V3 — it becomes a per-channel filter instead.
    let excluded_tools_for_channels: Option<toml::Value> =
        if let Some(toml::Value::Table(autonomy)) = table.get("autonomy") {
            autonomy.get("non_cli_excluded_tools").cloned()
        } else {
            None
        };

    // Collect V2 channel→agent bindings so they can be inverted onto agents
    // after the aliasing wrap. Stored as (channel_type, agent_alias) pairs.
    let mut agent_bindings: Vec<(String, String)> = Vec::new();

    // Wrap V2 flat channel configs in a "default" alias.
    // V2: [channels.discord] has flat fields (bot_token, enabled, …).
    // V3: [channels.discord.default] nests those fields one level deeper.
    // Detection: a V2 config has at least one non-table leaf at the top of the
    // channel-type table. A V3 config has only sub-tables (alias entries).
    for channels_key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*channels_key) {
            for ch_type in CHANNEL_TYPES {
                if let Some(toml::Value::Table(ch_table)) = channels.get(*ch_type)
                    && is_flat_channel_config(ch_table)
                {
                    let mut ch_clone = ch_table.clone();
                    // Strip V2 agent binding before wrapping; collect for inversion below.
                    if let Some(toml::Value::String(agent_alias)) = ch_clone.remove("agent") {
                        agent_bindings.push((ch_type.to_string(), agent_alias));
                    }
                    // Propagate non_cli_excluded_tools to channel-side excluded_tools.
                    if let Some(ref tools) = excluded_tools_for_channels {
                        ch_clone
                            .entry("excluded_tools".to_string())
                            .or_insert_with(|| tools.clone());
                    }
                    let mut alias_map = toml::Table::new();
                    alias_map.insert("default".to_string(), toml::Value::Table(ch_clone));
                    channels.insert(ch_type.to_string(), toml::Value::Table(alias_map));
                }
            }
        }
    }

    // Binding inversion — write channels = ["<type>.default"] onto each agent
    // that was bound to a channel via the V2 channels.<type>.agent field.
    // Only updates agents that already exist in the table; does not create skeletons
    // (an agent without provider/model cannot deserialize as DelegateAgentConfig).
    if !agent_bindings.is_empty()
        && let Some(toml::Value::Table(agents)) = table.get_mut("agents")
    {
        for (ch_type, agent_alias) in &agent_bindings {
            if let Some(toml::Value::Table(agent_table)) = agents.get_mut(agent_alias) {
                let ch_list = agent_table
                    .entry("channels".to_string())
                    .or_insert_with(|| toml::Value::Array(Vec::new()));
                if let toml::Value::Array(arr) = ch_list {
                    let binding = format!("{}.default", ch_type);
                    if !arr.iter().any(|v| v.as_str() == Some(binding.as_str())) {
                        arr.push(toml::Value::String(binding));
                    }
                }
            }
        }
    }

    // Synthesize [risk_profiles.default] from [autonomy] + [security.sandbox] +
    // [security.resources] if not already present.
    // non_cli_excluded_tools is propagated to per-channel excluded_tools above;
    // it does not survive in the risk profile.
    let autonomy_snapshot = table.get("autonomy").and_then(|v| v.as_table()).cloned();
    let sandbox_snapshot = table
        .get("security")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("sandbox"))
        .and_then(|v| v.as_table())
        .cloned();
    let resources_snapshot = table
        .get("security")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("resources"))
        .and_then(|v| v.as_table())
        .cloned();
    if autonomy_snapshot.is_some() || sandbox_snapshot.is_some() || resources_snapshot.is_some() {
        let risk_profiles = table
            .entry("risk_profiles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(profiles) = risk_profiles
            && !profiles.contains_key("default")
        {
            let mut profile = autonomy_snapshot.clone().unwrap_or_default();
            profile.remove("non_cli_excluded_tools");
            if let Some(sandbox) = &sandbox_snapshot {
                for (k, v) in sandbox {
                    let dest_key = if k == "enabled" {
                        "sandbox_enabled".to_string()
                    } else if k == "backend" {
                        "sandbox_backend".to_string()
                    } else {
                        k.clone()
                    };
                    profile.entry(dest_key).or_insert_with(|| v.clone());
                }
            }
            if let Some(resources) = &resources_snapshot {
                for (k, v) in resources {
                    profile.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            profiles.insert("default".to_string(), toml::Value::Table(profile));
        }
    }
    // Remove V2 flat [autonomy] block and the per-agent security subsections.
    table.remove("autonomy");
    if let Some(toml::Value::Table(security)) = table.get_mut("security") {
        security.remove("sandbox");
        security.remove("resources");
    }

    // Per-agent risk-profile carve-out.
    // Agents with inline max_depth or timeout overrides get their own risk profile
    // named after the agent alias.
    let agents_for_risk: Vec<(String, toml::Table)> =
        if let Some(toml::Value::Table(agents)) = table.get("agents") {
            agents
                .iter()
                .filter_map(|(alias, val)| val.as_table().map(|t| (alias.clone(), t.clone())))
                .collect()
        } else {
            Vec::new()
        };
    for (alias, agent_table) in &agents_for_risk {
        let has_overrides = agent_table.contains_key("max_depth")
            || agent_table.contains_key("timeout_secs")
            || agent_table.contains_key("agentic_timeout_secs");
        if !has_overrides {
            continue;
        }
        let risk_profiles = table
            .entry("risk_profiles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(profiles) = risk_profiles
            && !profiles.contains_key(alias.as_str())
        {
            let mut profile = toml::Table::new();
            for key in &["max_depth", "timeout_secs", "agentic_timeout_secs"] {
                if let Some(v) = agent_table.get(*key) {
                    let dest_key = match *key {
                        "max_depth" => "max_delegation_depth",
                        "timeout_secs" => "delegation_timeout_secs",
                        _ => key,
                    };
                    profile.insert(dest_key.to_string(), v.clone());
                }
            }
            profiles.insert(alias.clone(), toml::Value::Table(profile));
        }
    }

    // Synthesize [runtime_profiles.default] from [agent] if not already present.
    let agent_snapshot = table.get("agent").and_then(|v| v.as_table()).cloned();
    if let Some(agent) = agent_snapshot {
        let runtime_profiles = table
            .entry("runtime_profiles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(profiles) = runtime_profiles
            && !profiles.contains_key("default")
        {
            profiles.insert("default".to_string(), toml::Value::Table(agent));
        }
    }
    // Remove V2 flat [agent] block after synthesis.
    table.remove("agent");

    // Per-agent runtime-profile carve-out.
    let agents_for_runtime: Vec<(String, toml::Table)> =
        if let Some(toml::Value::Table(agents)) = table.get("agents") {
            agents
                .iter()
                .filter_map(|(alias, val)| val.as_table().map(|t| (alias.clone(), t.clone())))
                .collect()
        } else {
            Vec::new()
        };
    for (alias, agent_table) in &agents_for_runtime {
        let has_runtime_overrides =
            agent_table.contains_key("max_iterations") || agent_table.contains_key("agentic");
        if !has_runtime_overrides {
            continue;
        }
        let runtime_profiles = table
            .entry("runtime_profiles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(profiles) = runtime_profiles
            && !profiles.contains_key(alias.as_str())
        {
            let mut profile = toml::Table::new();
            if let Some(v) = agent_table.get("max_iterations") {
                profile.insert("max_tool_iterations".to_string(), v.clone());
            }
            if let Some(v) = agent_table.get("agentic") {
                profile.insert("agentic".to_string(), v.clone());
            }
            profiles.insert(alias.clone(), toml::Value::Table(profile));
        }
    }

    // Bundle namespace synthesis.
    // [memory_namespaces.default] — minimal entry seeded from V2 [memory].
    {
        let backend = table
            .get("memory")
            .and_then(|v| v.as_table())
            .and_then(|m| m.get("backend"))
            .cloned();
        let ns_map = table
            .entry("memory_namespaces")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(nst) = ns_map
            && !nst.contains_key("default")
        {
            let mut entry = toml::Table::new();
            entry.insert(
                "namespace".to_string(),
                toml::Value::String("default".to_string()),
            );
            if let Some(b) = backend {
                entry.insert("backend".to_string(), b);
            }
            nst.insert("default".to_string(), toml::Value::Table(entry));
        }
    }

    // [skill_bundles.default] ← directory = "skills" (default skills dir)
    {
        let bundles = table
            .entry("skill_bundles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(bt) = bundles
            && !bt.contains_key("default")
        {
            let mut b = toml::Table::new();
            b.insert(
                "directory".to_string(),
                toml::Value::String("skills".to_string()),
            );
            bt.insert("default".to_string(), toml::Value::Table(b));
        }
    }

    // Per-agent skill_bundle carve-out: agents with skills_directory get their own bundle.
    let agents_for_skills: Vec<(String, String)> =
        if let Some(toml::Value::Table(agents)) = table.get("agents") {
            agents
                .iter()
                .filter_map(|(alias, val)| {
                    val.as_table()
                        .and_then(|t| t.get("skills_directory"))
                        .and_then(|v| v.as_str())
                        .map(|dir| (alias.clone(), dir.to_string()))
                })
                .collect()
        } else {
            Vec::new()
        };
    for (alias, dir) in &agents_for_skills {
        let bundles = table
            .entry("skill_bundles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(bt) = bundles
            && !bt.contains_key(alias.as_str())
        {
            let mut b = toml::Table::new();
            b.insert("directory".to_string(), toml::Value::String(dir.clone()));
            bt.insert(alias.clone(), toml::Value::Table(b));
        }
    }

    // [knowledge_bundles.default] ← knowledge directory if present in [knowledge].
    if let Some(toml::Value::Table(knowledge)) = table.get("knowledge").cloned() {
        let bundles = table
            .entry("knowledge_bundles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(bt) = bundles
            && !bt.contains_key("default")
        {
            bt.insert("default".to_string(), toml::Value::Table(knowledge));
        }
    }

    // Migrate V2 [mcp.servers.<alias>] map format to V3 [[mcp.servers]] Vec format.
    // V2: servers are a TOML table keyed by name. V3: servers are an array with an explicit
    // `name` field. Collect the V2 entries first (for bundle synthesis below), then replace.
    let v2_mcp_servers: Option<Vec<(String, toml::Table)>> = table
        .get("mcp")
        .and_then(|v| v.as_table())
        .and_then(|mcp| mcp.get("servers"))
        .and_then(|v| v.as_table())
        .map(|servers| {
            servers
                .iter()
                .filter_map(|(alias, val)| val.as_table().map(|t| (alias.clone(), t.clone())))
                .collect()
        });
    if let Some(ref entries) = v2_mcp_servers
        && !entries.is_empty()
    {
        let mcp_table = table
            .entry("mcp")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(mcp) = mcp_table {
            let vec_entries: Vec<toml::Value> = entries
                .iter()
                .map(|(alias, server_t)| {
                    let mut t = server_t.clone();
                    t.entry("name".to_string())
                        .or_insert_with(|| toml::Value::String(alias.clone()));
                    toml::Value::Table(t)
                })
                .collect();
            mcp.insert("servers".to_string(), toml::Value::Array(vec_entries));
        }
    }

    // [mcp_bundles.default] ← lists all V2 mcp.servers.* alias keys.
    let mcp_server_aliases: Vec<String> = v2_mcp_servers
        .map(|entries| entries.into_iter().map(|(alias, _)| alias).collect())
        .unwrap_or_default();
    if !mcp_server_aliases.is_empty() {
        let bundles = table
            .entry("mcp_bundles")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(bt) = bundles
            && !bt.contains_key("default")
        {
            let mut b = toml::Table::new();
            b.insert(
                "servers".to_string(),
                toml::Value::Array(
                    mcp_server_aliases
                        .into_iter()
                        .map(toml::Value::String)
                        .collect(),
                ),
            );
            bt.insert("default".to_string(), toml::Value::Table(b));
        }
    }

    // Drop V2 [swarms.*] configs. The V3 swarm shape is incompatible and
    // the migration cannot safely synthesize V3 from V2 swarm definitions.
    if let Some(toml::Value::Table(swarms)) = table.get("swarms")
        && !swarms.is_empty()
    {
        tracing::warn!(
            "v2→v3 migration: dropping {} swarm configuration(s). \
             V3 swarms use a new shape — redefine them under [swarms.<alias>] \
             after upgrading.",
            swarms.len(),
        );
    }
    table.remove("swarms");

    // Rename legacy `channels_config` key to `channels`
    if table.contains_key("channels_config")
        && !table.contains_key("channels")
        && let Some(val) = table.remove("channels_config")
    {
        table.insert("channels".to_string(), val);
    }

    // Strip `enabled` from all channel alias configs and synthesize agent entries for
    // previously-enabled channels. Agent alias = "{ch_type}-{alias}" (e.g. "telegram-default").
    // Skip synthesis if the agent alias already exists in [agents].
    // Channels with `enabled = false` are stripped but no agent is synthesized.
    // Channels without an `enabled` key (V3-native configs) are treated as enabled.
    {
        use crate::helpers::validate_alias_key;

        let mut to_synthesize: Vec<(String, String)> = Vec::new(); // (agent_alias, channel_ref)

        if let Some(toml::Value::Table(channels)) = table.get_mut("channels") {
            for (ch_type, ch_val) in channels.iter_mut() {
                if let toml::Value::Table(alias_map) = ch_val {
                    for (alias, alias_val) in alias_map.iter_mut() {
                        if let toml::Value::Table(alias_cfg) = alias_val {
                            let was_disabled = alias_cfg
                                .remove("enabled")
                                .and_then(|v| v.as_bool())
                                .map(|b| !b)
                                .unwrap_or(false);

                            if !was_disabled {
                                let agent_alias = format!("{ch_type}-{alias}");
                                if validate_alias_key(&agent_alias).is_ok() {
                                    let ch_ref = format!("{ch_type}.{alias}");
                                    to_synthesize.push((agent_alias, ch_ref));
                                } else {
                                    tracing::warn!(
                                        "v2→v3 migration: skipping agent synthesis for \
                                         '{ch_type}.{alias}' — derived alias '{ch_type}-{alias}' \
                                         is invalid"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        if !to_synthesize.is_empty() {
            let agents = table
                .entry("agents")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(agents_map) = agents {
                for (agent_alias, ch_ref) in to_synthesize {
                    if !agents_map.contains_key(&agent_alias) {
                        let mut entry = toml::Table::new();
                        entry.insert(
                            "channels".to_string(),
                            toml::Value::Array(vec![toml::Value::String(ch_ref)]),
                        );
                        entry.insert("enabled".to_string(), toml::Value::Boolean(true));
                        agents_map.insert(agent_alias, toml::Value::Table(entry));
                    }
                }
            }
        }
    }

    // Rename any "claude-code" model-provider type key to "anthropic".
    if let Some(toml::Value::String(s)) = table.get_mut("default_provider")
        && s == "claude-code"
    {
        *s = "anthropic".to_string();
    }

    // Rename claude-code provider to anthropic, wrap flat provider entries into
    // `<type>.default` alias, and normalize providers.fallback to Vec<String> with
    // dotted type.alias keys (V2 had a bare string; V3 has an array of dotted paths).
    if let Some(toml::Value::Table(providers)) = table.get_mut("providers") {
        // Normalize providers.fallback to an array if it's a bare string (V2 shape).
        if let Some(toml::Value::String(s)) = providers.get("fallback").cloned() {
            let dotted = if s.contains('.') {
                s
            } else {
                format!("{s}.default")
            };
            providers.insert(
                "fallback".into(),
                toml::Value::Array(vec![toml::Value::String(dotted)]),
            );
        }
        if let Some(toml::Value::Table(models)) = providers.get_mut("models") {
            // Rename claude-code → anthropic.claude-code alias.
            if let Some(entry) = models.remove("claude-code") {
                tracing::info!(
                    "v2→v3 migration: moving [providers.models.claude-code] to \
                     [providers.models.anthropic.claude-code]. The anthropic provider \
                     supports OAuth tokens (sk-ant-oat01-) natively."
                );
                let alias_map = models
                    .entry("anthropic".to_string())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let toml::Value::Table(alias_map) = alias_map {
                    alias_map.entry("claude-code".to_string()).or_insert(entry);
                }
            }
            // Wrap flat [providers.models.<type>] entries into [providers.models.<type>.default].
            let type_keys: Vec<String> = models.keys().cloned().collect();
            for type_key in type_keys {
                if let Some(toml::Value::Table(entry)) = models.get(&type_key)
                    && is_flat_provider_config(entry)
                {
                    let flat = entry.clone();
                    let mut alias_map = toml::Table::new();
                    alias_map.insert("default".to_string(), toml::Value::Table(flat));
                    models.insert(type_key, toml::Value::Table(alias_map));
                }
            }
        }
    }

    // Synthesize per-agent [providers.models.<type>.<agent>] from inline agent fields.
    // Agents with inline brain fields that differ from the type's .default get their own alias.
    let agents_snapshot: Vec<(String, toml::Table)> = table
        .get("agents")
        .and_then(|v| v.as_table())
        .map(|agents| {
            agents
                .iter()
                .filter_map(|(alias, val)| val.as_table().map(|t| (alias.clone(), t.clone())))
                .collect()
        })
        .unwrap_or_default();

    for (agent_alias, agent_table) in &agents_snapshot {
        let provider_type = match agent_table.get("provider").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => continue,
        };

        let agent_model = agent_table
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let agent_temp = agent_table.get("temperature").cloned();
        let agent_api_key = agent_table
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let default_entry: Option<toml::Table> = table
            .get("providers")
            .and_then(|v| v.as_table())
            .and_then(|p| p.get("models"))
            .and_then(|v| v.as_table())
            .and_then(|m| m.get(&provider_type))
            .and_then(|v| v.as_table())
            .and_then(|alias_map| alias_map.get("default"))
            .and_then(|v| v.as_table())
            .cloned();

        let default_model = default_entry
            .as_ref()
            .and_then(|t| t.get("model"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let default_api_key = default_entry
            .as_ref()
            .and_then(|t| t.get("api_key"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let default_temp = default_entry
            .as_ref()
            .and_then(|t| t.get("temperature"))
            .cloned();

        let model_differs = !agent_model.is_empty() && agent_model != default_model;
        let api_key_differs = !agent_api_key.is_empty() && agent_api_key != default_api_key;
        let temp_differs = agent_temp.is_some() && agent_temp != default_temp;

        let (alias_name, needs_new_entry) = if model_differs || api_key_differs || temp_differs {
            (agent_alias.clone(), true)
        } else {
            ("default".to_string(), false)
        };

        let model_provider_value = format!("{provider_type}.{alias_name}");

        if needs_new_entry {
            let mut new_entry = default_entry.clone().unwrap_or_default();
            if !agent_model.is_empty() {
                new_entry.insert("model".to_string(), toml::Value::String(agent_model));
            }
            if !agent_api_key.is_empty() {
                new_entry.insert("api_key".to_string(), toml::Value::String(agent_api_key));
            }
            if let Some(temp) = agent_temp {
                new_entry.insert("temperature".to_string(), temp);
            }
            let providers = table
                .entry("providers")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(p) = providers {
                let models = p
                    .entry("models")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let toml::Value::Table(m) = models {
                    let alias_map = m
                        .entry(provider_type.clone())
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                    if let toml::Value::Table(am) = alias_map {
                        am.entry(alias_name)
                            .or_insert_with(|| toml::Value::Table(new_entry));
                    }
                }
            }
        }

        if let Some(toml::Value::Table(agents)) = table.get_mut("agents")
            && let Some(toml::Value::Table(at)) = agents.get_mut(agent_alias)
        {
            at.insert(
                "model_provider".to_string(),
                toml::Value::String(model_provider_value),
            );
            at.remove("provider");
            at.remove("model");
            at.remove("temperature");
            at.remove("api_key");
        }
    }

    // Drop the global `[cost.prices.*]` table. Pricing now lives on each
    // `[providers.models.<provider>.<alias>]` block.
    if let Some(toml::Value::Table(cost)) = table.get_mut("cost")
        && let Some(toml::Value::Table(prices)) = cost.remove("prices")
    {
        for (model, entry) in prices {
            let (input, output) = match &entry {
                toml::Value::Table(t) => (
                    t.get("input")
                        .and_then(toml::Value::as_float)
                        .unwrap_or(0.0),
                    t.get("output")
                        .and_then(toml::Value::as_float)
                        .unwrap_or(0.0),
                ),
                _ => (0.0, 0.0),
            };
            tracing::info!(
                model = %model,
                input = input,
                output = output,
                "v2→v3 migration: dropping cost.prices.{model} (input={input}, \
                 output={output}). Pricing now lives under \
                 [providers.models.<provider>.<alias>.pricing] — re-enter the \
                 values under the alias that uses this model."
            );
        }
    }

    // Migrate legacy top-level [memory] pgvector fields to [memory.postgres]
    // and db_url to [storage.provider.config].
    let (legacy_pg_enabled, legacy_pg_dims, legacy_db_url) =
        if let Some(toml::Value::Table(memory)) = table.get_mut("memory") {
            (
                memory.remove("pgvector_enabled"),
                memory.remove("pgvector_dimensions"),
                memory.remove("db_url"),
            )
        } else {
            (None, None, None)
        };

    if (legacy_pg_enabled.is_some() || legacy_pg_dims.is_some())
        && let Some(toml::Value::Table(memory)) = table.get_mut("memory")
    {
        let postgres = memory
            .entry("postgres")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(pg) = postgres {
            if let Some(v) = legacy_pg_enabled {
                pg.entry("vector_enabled").or_insert(v);
            }
            if let Some(v) = legacy_pg_dims {
                pg.entry("vector_dimensions").or_insert(v);
            }
        }
    }

    if let Some(url) = legacy_db_url {
        let storage = table
            .entry("storage")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(s) = storage {
            let provider = s
                .entry("provider")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(p) = provider {
                let cfg = p
                    .entry("config")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let toml::Value::Table(c) = cfg {
                    c.entry("db_url").or_insert(url);
                }
            }
        }
    }
}

// ── File-level migration (comment-preserving) ───────────────────────────────
//
// Uses V1Compat (the single source of migration logic) to compute the migrated
// Config, then syncs the original toml_edit document to match. The sync function
// is generic — it doesn't know field names, it just diffs two table structures.

/// Migrate a raw TOML config file, preserving comments and formatting.
/// Returns `None` if already at current version.
pub fn migrate_file(raw: &str) -> Result<Option<String>> {
    let mut table: toml::Table = toml::from_str(raw).context("Failed to parse config table")?;
    prepare_table(&mut table);
    let prepared = toml::to_string(&table).context("Failed to re-serialize prepared table")?;
    let compat: V1Compat = toml::from_str(&prepared).context("Failed to deserialize config")?;
    if compat.config.schema_version >= CURRENT_SCHEMA_VERSION && !compat.has_legacy_fields() {
        return Ok(None);
    }
    let config = compat.into_config();

    // Serialize the migrated config to get the target table structure.
    let target: toml::Table = toml::from_str(&toml::to_string(&config)?)
        .context("Failed to round-trip migrated config")?;

    // Sync the original document (with comments) to match the target.
    let mut doc: DocumentMut = raw.parse().context("Failed to parse config.toml")?;

    // Rename channels_config → channels in the document to preserve comments.
    if doc.contains_key("channels_config")
        && !doc.contains_key("channels")
        && let Some(val) = doc.remove("channels_config")
    {
        doc.insert("channels", val);
    }

    sync_table(doc.as_table_mut(), &target);

    Ok(Some(doc.to_string()))
}

/// Recursively sync a `toml_edit` table to match a target `toml::Table`.
/// - Keys absent from target are removed.
/// - Keys present in target but not in doc are inserted.
/// - Sub-tables are recursed. Leaf values are updated only if changed.
/// - Unchanged entries retain their original formatting and comments.
pub fn sync_table(doc: &mut toml_edit::Table, target: &toml::Table) {
    // Remove keys not in target.
    let to_remove: Vec<String> = doc
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !target.contains_key(k))
        .collect();
    for key in &to_remove {
        doc.remove(key);
    }

    // Add or update keys from target.
    for (key, target_value) in target {
        match target_value {
            toml::Value::Table(sub_target) => {
                let entry = doc
                    .entry(key)
                    .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                if let Some(sub_doc) = entry.as_table_mut() {
                    sync_table(sub_doc, sub_target);
                }
            }
            _ => {
                if let Some(existing) = doc.get(key).and_then(|i| i.as_value()) {
                    // Compare raw values, ignoring formatting/comments.
                    if values_equal(existing, target_value) {
                        continue;
                    }
                }
                doc.insert(key, toml_edit::value(toml_to_edit_value(target_value)));
            }
        }
    }
}

/// Compare a `toml_edit::Value` and a `toml::Value` for semantic equality,
/// ignoring formatting, whitespace, and comments.
fn values_equal(edit: &toml_edit::Value, toml: &toml::Value) -> bool {
    match (edit, toml) {
        (toml_edit::Value::String(a), toml::Value::String(b)) => a.value() == b,
        (toml_edit::Value::Integer(a), toml::Value::Integer(b)) => a.value() == b,
        (toml_edit::Value::Float(a), toml::Value::Float(b)) => (a.value() - b).abs() < f64::EPSILON,
        (toml_edit::Value::Boolean(a), toml::Value::Boolean(b)) => a.value() == b,
        (toml_edit::Value::Array(a), toml::Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(ae, be)| values_equal(ae, be))
        }
        _ => false,
    }
}

/// Generate a canonical mock config for the given schema version by constructing
/// a V1Compat mock in code and running the migration chain up to `version`.
/// Returns an error if `version` is not a supported schema version.
/// Deep-merge `overlay` into `base`, with overlay values winning.
/// Sub-tables are merged recursively; scalars and arrays are replaced.
fn merge_tables(base: &mut toml::Table, overlay: &toml::Table) {
    for (k, v) in overlay {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(base_t)), toml::Value::Table(overlay_t)) => {
                merge_tables(base_t, overlay_t);
            }
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Generate a canonical mock config for the given schema version.
///
/// `fixture_raw` is the content of `v1.toml` (the base fixture).
/// `partial_raw` is the optional content of `v{version}.partial.toml`, deep-merged
/// on top of the result after the migration chain runs.
pub fn generate_fixture(
    version: u32,
    fixture_raw: &str,
    partial_raw: Option<&str>,
) -> Result<String> {
    type MigrateFn = fn(&mut V1Compat);
    type SnapshotFn = fn(&V1Compat) -> toml::Table;

    // Each entry: (version, migration step, snapshot serializer).
    // Migrations are cumulative. To add a new version: append one line.
    // Version 1 is handled before this loop — raw fixture pass-through, no migration.
    let steps: &[(u32, MigrateFn, SnapshotFn)] = &[
        (2, |c| c.v1_to_v2(), V1Compat::snapshot_v2),
        (
            CURRENT_SCHEMA_VERSION,
            |c| v2_to_v3(&mut c.config),
            V1Compat::snapshot_current,
        ),
    ];

    let max = steps.iter().map(|&(v, _, _)| v).max().unwrap_or(0);
    if version < 1 || version > max {
        return Err(anyhow::anyhow!(
            "unsupported schema version {version}; supported versions are 1–{max}"
        ));
    }

    // V1 output is the raw fixture verbatim — no migration, no prepare_table.
    // Parsing the fixture through from_v1_fixture (which runs prepare_table) would
    // irreversibly rewrite field names (room_id → allowed_rooms, etc.), producing
    // a V3-shaped document instead of the true V1 shape.
    if version == 1 {
        let mut table: toml::Table =
            toml::from_str(fixture_raw).context("failed to parse v1 fixture")?;
        table.insert("schema_version".into(), toml::Value::Integer(1));
        if let Some(partial_toml) = partial_raw {
            let overlay: toml::Table =
                toml::from_str(partial_toml).context("failed to parse partial fixture")?;
            merge_tables(&mut table, &overlay);
        }
        return toml::to_string_pretty(&table).context("failed to serialize fixture");
    }

    let mut compat: V1Compat =
        V1Compat::from_v1_fixture(fixture_raw).map_err(|e| anyhow::anyhow!(e))?;

    let mut snapshot: SnapshotFn = V1Compat::snapshot_current;
    for &(target, migrate, serialize) in steps {
        if version >= target {
            migrate(&mut compat);
            snapshot = serialize;
        }
    }
    compat.config.schema_version = version;

    let mut table = snapshot(&compat);

    if let Some(partial_toml) = partial_raw {
        let overlay: toml::Table =
            toml::from_str(partial_toml).context("failed to parse partial fixture")?;
        merge_tables(&mut table, &overlay);
    }

    toml::to_string_pretty(&table).context("failed to serialize fixture")
}

/// Convert a `toml::Value` to a `toml_edit::Value`.
fn toml_to_edit_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::String(s) => toml_edit::Value::from(s.as_str()),
        toml::Value::Integer(i) => toml_edit::Value::from(*i),
        toml::Value::Float(f) => toml_edit::Value::from(*f),
        toml::Value::Boolean(b) => toml_edit::Value::from(*b),
        toml::Value::Array(arr) => {
            let mut a = toml_edit::Array::new();
            for item in arr {
                a.push(toml_to_edit_value(item));
            }
            toml_edit::Value::Array(a)
        }
        toml::Value::Datetime(dt) => dt
            .to_string()
            .parse()
            .unwrap_or_else(|_| toml_edit::Value::from(dt.to_string())),
        toml::Value::Table(tbl) => {
            // Tables inside arrays (e.g. `[[providers.model_routes]]`) need to be
            // converted to inline tables so they can be pushed into a toml_edit Array.
            let mut inline = toml_edit::InlineTable::new();
            for (k, v) in tbl {
                inline.insert(k, toml_to_edit_value(v));
            }
            toml_edit::Value::InlineTable(inline)
        }
    }
}
