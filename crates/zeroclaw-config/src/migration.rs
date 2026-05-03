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
//!    function. All shape-changing logic lives at the TOML level so a single
//!    `prepare_table` pass produces a current-schema table that deserializes
//!    directly into `Config`. `prepare_table()` dispatches to these based on
//!    the input's `schema_version` value.
//! 3. Add a test in `tests/component/config_migration.rs`:
//!    - Run the test through `migration::migrate_to_current(toml_str)`.
//!    - Assert the migrated `Config` has values in the new locations.
//!    - Assert the old locations are empty/cleared.
//! 4. Verify with `cargo test --test component -- config_migration`.
//!
//! ## User-facing migration command
//!
//! `zeroclaw config migrate` rewrites the on-disk `config.toml` to the current
//! schema version using `toml_edit` to preserve comments and formatting.

use anyhow::{Context, Result};
use toml_edit::DocumentMut;

pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Top-level keys from V1 that `migrate_v1_provider_fields_into_providers_table`
/// folds into the V2/V3 `[providers.*]` shape. Used by the unknown-key
/// detector to suppress false "unknown key" warnings on partially-migrated
/// inputs (and by `migrate_file` to short-circuit no-op writes).
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
    // V2 keys removed in V3 — migration absorbs them into risk_profiles +
    // providers.tts + storage + scheduler. Suppressing them in the
    // unknown-key detector avoids false-positive warnings during V2→V3
    // migration of partially-shaped inputs.
    "autonomy",
    "agent",
    // Swarm support has been removed; it will return in a future release.
    // The V2→V3 migration drops `[swarms.*]` tables, but keep this here so
    // the unknown-key detector doesn't warn before that drop runs.
    "swarms",
];

/// Run `prepare_table` on a raw config TOML string and deserialize the
/// result directly into `Config`. This is the canonical migration entry
/// point — V1 / V2 / V3 inputs all flow through the same path; the
/// per-version TOML transforms in `prepare_table` ensure deserialization
/// always sees a current-schema-shaped table.
///
/// Returns a fully-migrated `Config` with `schema_version` bumped to
/// `CURRENT_SCHEMA_VERSION`.
pub fn migrate_to_current(raw: &str) -> Result<super::schema::Config> {
    let mut table: toml::Table = toml::from_str(raw).context("failed to parse config TOML")?;
    prepare_table(&mut table);
    let prepared = toml::to_string(&table).context("failed to re-serialize prepared table")?;
    let mut config: super::schema::Config =
        toml::from_str(&prepared).context("failed to deserialize migrated config")?;
    config.schema_version = CURRENT_SCHEMA_VERSION;
    Ok(config)
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

/// V1 → V2 provider-section migration. Lifts V1's flat top-level
/// provider scheme onto V2's nested `[providers.models.<type>.<alias>]`
/// shape entirely at the TOML level, replacing the legacy `V1Compat`
/// in-memory pass with a single pre-deserialization transform.
///
/// Handles:
/// 1. `[model_providers.<type>]` → `[providers.models.<type>.default]`
///    (V1 had a single alias per type; default-named).
/// 2. Top-level V1 scalars (`api_key`, `api_url`, `api_path`,
///    `default_model`, `default_temperature`, `provider_timeout_secs`,
///    `provider_max_tokens`, `extra_headers`) folded into
///    `[providers.models.<type>.default]` for the type resolved from
///    `default_provider` (or `model_provider` alias) → first existing
///    provider type → literal `"default"`.
/// 3. Top-level `[[model_routes]]` / `[[embedding_routes]]` lifted under
///    `[providers]` if not already present there.
/// 4. All consumed V1 keys removed from the top-level table so the
///    serde deserializer doesn't see leftover legacy fields.
///
/// No-op when no V1 keys are present (V2/V3 inputs pass through unchanged).
fn migrate_v1_provider_fields_into_providers_table(table: &mut toml::Table) {
    // Snapshot top-level V1 keys before mutating anything.
    let api_key = table.remove("api_key");
    let api_url = table.remove("api_url");
    let api_path = table.remove("api_path");
    let default_model = table
        .remove("default_model")
        .or_else(|| table.remove("model"));
    let default_temperature = table.remove("default_temperature");
    let provider_timeout_secs = table.remove("provider_timeout_secs");
    let provider_max_tokens = table.remove("provider_max_tokens");
    let extra_headers = table.remove("extra_headers");
    // Resolve the V1 default-provider name. Pre-apply the V1's
    // `claude-code` → `anthropic` rename here so the synthesized
    // entry lands at `providers.models.anthropic.default` rather than
    // tripping the v2_to_v3 alias-rename block (which would otherwise
    // see `providers.models.claude-code` and shove it under
    // `anthropic.claude-code`, double-nesting the value).
    let default_provider = table
        .remove("default_provider")
        .or_else(|| table.remove("model_provider"))
        .and_then(|v| v.as_str().map(str::to_string))
        .map(|s| {
            if s == "claude-code" {
                "anthropic".to_string()
            } else {
                s
            }
        });
    let v1_model_routes = table.remove("model_routes");
    let v1_embedding_routes = table.remove("embedding_routes");
    let v1_model_providers = table.remove("model_providers");

    // Ensure [providers] exists; we'll write everything under it.
    let providers = table
        .entry("providers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(providers) = providers else {
        return;
    };
    let models = providers
        .entry("models")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(models) = models else {
        return;
    };

    // 1. [model_providers.<type>] → [providers.models.<type>.default].
    if let Some(toml::Value::Table(legacy_providers)) = v1_model_providers {
        for (ty, profile) in legacy_providers {
            if let toml::Value::Table(profile) = profile {
                let alias_map = models
                    .entry(ty)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let toml::Value::Table(alias_map) = alias_map {
                    alias_map
                        .entry("default".to_string())
                        .or_insert(toml::Value::Table(profile));
                }
            }
        }
    }

    // 2. Synthesize [providers.models.<type>.default] from top-level V1 scalars.
    let has_v1_scalars = api_key.is_some()
        || api_url.is_some()
        || api_path.is_some()
        || default_model.is_some()
        || default_temperature.is_some()
        || provider_timeout_secs.is_some()
        || provider_max_tokens.is_some()
        || extra_headers
            .as_ref()
            .is_some_and(|v| v.as_table().is_some_and(|t| !t.is_empty()));

    if has_v1_scalars || default_provider.is_some() {
        let provider_type = default_provider
            .clone()
            .or_else(|| models.keys().next().cloned())
            .unwrap_or_else(|| "default".to_string());
        let alias_map = models
            .entry(provider_type)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(alias_map) = alias_map {
            let entry = alias_map
                .entry("default".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(entry) = entry {
                // (V1 scalar key, V2 entry key) — fill only when absent so
                // user-supplied [providers.models.<type>.default] wins.
                let pairs: &[(Option<toml::Value>, &str)] = &[
                    (api_key, "api_key"),
                    // V1's `api_url` is V2's `base_url`.
                    (api_url, "base_url"),
                    (api_path, "api_path"),
                    (default_model, "model"),
                    (default_temperature, "temperature"),
                    (provider_timeout_secs, "timeout_secs"),
                    (provider_max_tokens, "max_tokens"),
                    (extra_headers, "extra_headers"),
                ];
                for (value, dest_key) in pairs {
                    if let Some(v) = value
                        && !entry.contains_key(*dest_key)
                    {
                        entry.insert((*dest_key).to_string(), v.clone());
                    }
                }
            }
        }
    }

    // 3. Lift V1 top-level [[model_routes]] / [[embedding_routes]] under [providers].
    if let Some(routes) = v1_model_routes
        && !providers.contains_key("model_routes")
    {
        providers.insert("model_routes".to_string(), routes);
    }
    if let Some(routes) = v1_embedding_routes
        && !providers.contains_key("embedding_routes")
    {
        providers.insert("embedding_routes".to_string(), routes);
    }
}

/// Schema-derived list of channel types recognised in V2 `[channels]` that the
/// aliasing migration wraps into `[channels.<type>.default]`. Pulled from
/// `Config::map_key_sections()` so adding a new channel to the schema
/// automatically extends migration coverage — no hand-maintained list to
/// drift out of sync.
///
/// `map_key_sections()` emits kebab-case (e.g. `channels.gmail-push`); the
/// TOML keys are snake_case (`gmail_push`). Convert back so comparison against
/// raw TOML table keys works.
fn channel_types() -> Vec<String> {
    use crate::schema::Config;
    Config::map_key_sections()
        .into_iter()
        .filter_map(|s| {
            // Sections under `channels.<type>` (alias maps) — extract `<type>`.
            let rest = s.path.strip_prefix("channels.")?;
            // Skip nested deeper paths like `channels.<type>.<sub>` if any
            // ever appear; only direct children of `channels.` are types.
            if rest.contains('.') {
                None
            } else {
                // Kebab → snake to match TOML key shape.
                Some(rest.replace('-', "_"))
            }
        })
        .collect()
}

/// Move every key from V2's global `[agent]` table onto `[agents.default]`,
/// then delete `[agent]`. User-supplied keys on `[agents.default]` win
/// (we only insert when absent). No-op when `[agent]` doesn't exist.
fn fold_v2_agent_into_default_agent(table: &mut toml::Table) {
    let Some(toml::Value::Table(legacy_agent)) = table.remove("agent") else {
        return;
    };
    let agents = table
        .entry("agents")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(agents_map) = agents else {
        return;
    };
    let default_agent = agents_map
        .entry("default")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(default_agent_table) = default_agent else {
        return;
    };
    for (k, v) in legacy_agent {
        default_agent_table.entry(k).or_insert(v);
    }
}

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

/// Pre-deserialization table migration dispatcher.
///
/// Reads `schema_version` from the raw table and calls only the transforms
/// needed to bring it up to the current schema. Each version function is
/// responsible for all TOML-level shape changes between those two versions.
/// Called before deserialization into `Config`. Wrapped by
/// `migrate_to_current` for the common case.
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

/// V1 → V2 TOML-level transforms: scalar field renames + V1 top-level
/// scalar/section migrations into the V2 `[providers.*]` shape. All work
/// happens at the TOML level so deserialization into `Config` succeeds
/// directly — no V1Compat intermediate struct is needed.
fn v1_to_v2_table(table: &mut toml::Table) {
    // ── V1 top-level provider migration ─────────────────────────────
    // Move [model_providers.<type>] → [providers.models.<type>.default],
    // synthesize a default-provider entry from V1 top-level scalars
    // (api_key, default_model, etc.), and lift V1 top-level
    // [[model_routes]] / [[embedding_routes]] under [providers].
    migrate_v1_provider_fields_into_providers_table(table);

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
/// removals, and renames that must happen before deserialization into `Config`.
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
    let known_channel_types = channel_types();
    for channels_key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*channels_key) {
            for ch_type in &known_channel_types {
                if let Some(toml::Value::Table(ch_table)) = channels.get(ch_type.as_str())
                    && is_flat_channel_config(ch_table)
                {
                    let mut ch_clone = ch_table.clone();
                    // Strip V2 agent binding before wrapping; collect for inversion below.
                    if let Some(toml::Value::String(agent_alias)) = ch_clone.remove("agent") {
                        agent_bindings.push((ch_type.clone(), agent_alias));
                    }
                    // Propagate non_cli_excluded_tools to channel-side excluded_tools.
                    if let Some(ref tools) = excluded_tools_for_channels {
                        ch_clone
                            .entry("excluded_tools".to_string())
                            .or_insert_with(|| tools.clone());
                    }
                    let mut alias_map = toml::Table::new();
                    alias_map.insert("default".to_string(), toml::Value::Table(ch_clone));
                    channels.insert(ch_type.clone(), toml::Value::Table(alias_map));
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
    // Fold what's left of V2's global `[agent]` table onto
    // `[agents.default]` (after risk/runtime profile synthesis has copied
    // out what it needed). `AgentConfig` (singular) was deleted in V3 —
    // every former global runtime tunable is per-agent now. User-supplied
    // [agents.default] always wins. The fold internally `remove`s
    // `[agent]`, so no separate cleanup is needed below. Idempotent on
    // V3 inputs.
    fold_v2_agent_into_default_agent(table);

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

    // Drop legacy `[swarms.*]` configs. Swarm support has been removed; it
    // will return in a follow-up release with a new shape.
    if let Some(toml::Value::Table(swarms)) = table.get("swarms")
        && !swarms.is_empty()
    {
        tracing::warn!(
            "v2→v3 migration: dropping {} swarm configuration(s). \
             Swarm support has been removed and will return in a future release.",
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

    // Rename claude-code provider to anthropic and wrap flat provider entries
    // into `<type>.default` alias. The legacy `providers.fallback` field is
    // gone in V3 — strip it from any V2 input that still carries one.
    if let Some(toml::Value::Table(providers)) = table.get_mut("providers") {
        providers.remove("fallback");
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

    // V3 storage promotion:
    // V2 had typed memory backends as `[memory.<backend>]` subsections plus
    // a single-instance `[storage.provider.config]` table for connection params.
    // V3 collapses those into alias-keyed `[storage.<backend>.<alias>]`.
    promote_v2_storage_subsystem(table);

    // V3 TTS promotion:
    // V2 had per-backend subsections under `[tts.<backend>]`. V3 promotes
    // to `[providers.tts.<backend>.<alias>]` with a single union shape.
    promote_v2_tts_subsystem(table);

    // V3 cron promotion:
    // - V2 had `[cron]` with subsystem knobs (enabled, catch_up_on_startup,
    //   max_run_history) plus `[[cron.jobs]]` array. V3 makes `[cron.<alias>]`
    //   the alias-keyed job map directly; subsystem knobs live on `[scheduler]`.
    // - Move scalar cron fields onto `[scheduler]`; user-supplied scheduler
    //   values always win.
    // - Move `[[cron.jobs]]` array into `[cron.<id>]` map (id field removed,
    //   the alias key preserves stable identity).
    promote_v2_cron_subsystem(table);
}

fn promote_v2_storage_subsystem(table: &mut toml::Table) {
    use std::collections::BTreeMap;

    // Step 1: extract V2 inputs.
    //
    // V2 sources for migration:
    // - [memory] sqlite_open_timeout_secs
    // - [memory] pgvector_enabled, pgvector_dimensions, db_url (legacy)
    // - [memory.postgres] vector_enabled, vector_dimensions
    // - [memory.qdrant] url, collection, api_key
    // - [storage.provider.config] db_url, schema, table, connect_timeout_secs, provider
    //
    // V3 destinations:
    // - [storage.sqlite.default] open_timeout_secs, path
    // - [storage.postgres.default] db_url, schema, table, connect_timeout_secs, vector_enabled, vector_dimensions
    // - [storage.qdrant.default] url, collection, api_key
    // - [storage.markdown.default] directory
    // - [storage.lucid.default] binary_path

    let mut sqlite_default: BTreeMap<String, toml::Value> = BTreeMap::new();
    let mut postgres_default: BTreeMap<String, toml::Value> = BTreeMap::new();
    let mut qdrant_default: BTreeMap<String, toml::Value> = BTreeMap::new();

    if let Some(toml::Value::Table(memory)) = table.get_mut("memory") {
        // SQLite open-timeout migrates onto storage.sqlite.default.open_timeout_secs.
        if let Some(v) = memory.remove("sqlite_open_timeout_secs") {
            sqlite_default.insert("open_timeout_secs".to_string(), v);
        }

        // V1-shaped pgvector fields onto storage.postgres.default.
        if let Some(v) = memory.remove("pgvector_enabled") {
            postgres_default.insert("vector_enabled".to_string(), v);
        }
        if let Some(v) = memory.remove("pgvector_dimensions") {
            postgres_default.insert("vector_dimensions".to_string(), v);
        }

        // Legacy V1 [memory] db_url onto storage.postgres.default.
        if let Some(v) = memory.remove("db_url") {
            postgres_default.insert("db_url".to_string(), v);
        }

        // V2 [memory.postgres] vector fields.
        if let Some(toml::Value::Table(pg)) = memory.remove("postgres") {
            for (k, v) in pg {
                postgres_default.entry(k).or_insert(v);
            }
        }

        // V2 [memory.qdrant] full subsection.
        if let Some(toml::Value::Table(qd)) = memory.remove("qdrant") {
            for (k, v) in qd {
                qdrant_default.entry(k).or_insert(v);
            }
        }
    }

    // V2 [storage.provider.config] connection block. The `provider = "..."`
    // field was a V2 redundancy (memory.backend already names the backend);
    // we drop it on the floor here. The remaining fields fold onto
    // storage.postgres.default (the only V2 SQL backend that used them).
    if let Some(toml::Value::Table(storage)) = table.get_mut("storage")
        && let Some(toml::Value::Table(provider)) = storage.remove("provider")
    {
        let provider_config = provider
            .into_iter()
            .find_map(|(k, v)| {
                if k == "config"
                    && let toml::Value::Table(t) = v
                {
                    return Some(t);
                }
                None
            })
            .unwrap_or_default();
        for (k, v) in provider_config {
            if k == "provider" {
                continue;
            }
            postgres_default.entry(k).or_insert(v);
        }
    }

    // Step 2: write the synthesized defaults into [storage.<backend>.default]
    // entries. Preserves any user-supplied V3 `[storage.<backend>.<alias>]`
    // entries already present (we only fill `default` if missing or merge into
    // it without overwrite).
    let storage_root = table
        .entry("storage")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(storage) = storage_root else {
        return;
    };

    let merge_into_default =
        |storage: &mut toml::Table, backend: &str, fields: BTreeMap<String, toml::Value>| {
            if fields.is_empty() {
                return;
            }
            let backend_table = storage
                .entry(backend.to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(bt) = backend_table {
                let default_entry = bt
                    .entry("default".to_string())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let toml::Value::Table(de) = default_entry {
                    for (k, v) in fields {
                        de.entry(k).or_insert(v);
                    }
                }
            }
        };

    merge_into_default(storage, "sqlite", sqlite_default);
    merge_into_default(storage, "postgres", postgres_default);
    merge_into_default(storage, "qdrant", qdrant_default);
}

fn promote_v2_tts_subsystem(table: &mut toml::Table) {
    // V2 backends → V3 alias map. Old field names map onto the unified
    // TtsProviderConfig:
    // - openai:     api_key, model, speed                  → api_key, model, speed
    // - elevenlabs: api_key, model_id, stability, similarity_boost
    //                                                        → api_key, model, stability, similarity_boost
    // - google:     api_key, language_code                 → api_key, language_code
    // - edge:       binary_path                            → binary_path
    // - piper:      api_url                                → api_url
    const BACKENDS: &[&str] = &["openai", "elevenlabs", "google", "edge", "piper"];

    let mut promoted: std::collections::BTreeMap<&'static str, toml::Table> =
        std::collections::BTreeMap::new();

    if let Some(toml::Value::Table(tts)) = table.get_mut("tts") {
        for &backend in BACKENDS {
            let Some(toml::Value::Table(mut entry)) = tts.remove(backend) else {
                continue;
            };
            // V2 elevenlabs.model_id → V3 generic .model field.
            if backend == "elevenlabs"
                && let Some(model_id) = entry.remove("model_id")
            {
                entry.entry("model".to_string()).or_insert(model_id);
            }
            promoted.insert(backend, entry);
        }
        // V2 had `default_provider = "openai"` (bare type). V3 carries
        // a dotted alias ref. Upgrade in place when the value is a known
        // bare backend name; leave dotted forms (V3-shaped) untouched.
        if let Some(toml::Value::String(s)) = tts.get_mut("default_provider")
            && BACKENDS.contains(&s.as_str())
        {
            *s = format!("{s}.default");
        }
        // V2 default_voice can be promoted to the active alias's voice
        // override when the V3 alias doesn't already specify one. This is
        // best-effort — only the inferred default-provider alias gets the
        // voice. Other instances retain their own voice or fall back to
        // [tts].default_voice at runtime.
        let v2_default_voice = tts
            .get("default_voice")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(voice) = v2_default_voice {
            // Attempt to promote onto the dotted-alias entry resolved from
            // default_provider. Look up the freshly-written value (post bare→dotted upgrade).
            if let Some(toml::Value::String(dotted)) = tts.get("default_provider").cloned()
                && let Some((ty, _alias)) = dotted.split_once('.')
                && let Some(entry) = promoted.get_mut(ty)
            {
                entry
                    .entry("voice".to_string())
                    .or_insert(toml::Value::String(voice));
            }
        }
    }

    if promoted.is_empty() {
        return;
    }

    let providers_root = table
        .entry("providers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(providers) = providers_root else {
        return;
    };
    let tts_map_root = providers
        .entry("tts")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(tts_map) = tts_map_root else {
        return;
    };

    for (backend, entry) in promoted {
        let backend_root = tts_map
            .entry(backend.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(bt) = backend_root {
            let default_entry = bt
                .entry("default".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(de) = default_entry {
                for (k, v) in entry {
                    de.entry(k).or_insert(v);
                }
            }
        }
    }
}

fn promote_v2_cron_subsystem(table: &mut toml::Table) {
    // Detect V3-shape early: if `[cron]` is already a map of subtables and
    // contains no subsystem scalars / jobs array, no work to do (idempotent).
    let cron_is_v3_shape = match table.get("cron") {
        Some(toml::Value::Table(t)) => {
            !t.contains_key("enabled")
                && !t.contains_key("catch_up_on_startup")
                && !t.contains_key("max_run_history")
                && !t.contains_key("jobs")
        }
        _ => true, // missing or non-table → nothing to migrate
    };
    if cron_is_v3_shape {
        return;
    }

    // Take ownership of the V2 cron table so we can re-shape it from scratch.
    let v2_cron = match table.remove("cron") {
        Some(toml::Value::Table(t)) => t,
        Some(other) => {
            tracing::warn!(
                "v2→v3 migration: [cron] is not a table, dropping ({:?})",
                other.type_str()
            );
            return;
        }
        None => return,
    };

    let mut cron_subsystem: toml::Table = toml::Table::new();
    let mut jobs_array: Option<toml::Value> = None;
    let mut new_cron_map: toml::Table = toml::Table::new();

    for (k, v) in v2_cron {
        match k.as_str() {
            "enabled" | "catch_up_on_startup" | "max_run_history" => {
                cron_subsystem.insert(k, v);
            }
            "jobs" => {
                jobs_array = Some(v);
            }
            // V3 already-aliased entries: preserve.
            _ => {
                new_cron_map.insert(k, v);
            }
        }
    }

    // Promote subsystem scalars onto `[scheduler]`. User-supplied values win.
    if !cron_subsystem.is_empty() {
        let scheduler = table
            .entry("scheduler")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(s) = scheduler {
            for (k, v) in cron_subsystem {
                s.entry(k).or_insert(v);
            }
        }
    }

    // Convert `[[cron.jobs]]` array into the alias-keyed map. Each entry's
    // `id` field becomes the map key; if missing, synthesize one from the
    // index. Conflicts with already-present V3 aliases keep the V3 entry
    // (idempotent on V3-shaped configs that also had V2 jobs).
    if let Some(toml::Value::Array(items)) = jobs_array {
        for (idx, item) in items.into_iter().enumerate() {
            if let toml::Value::Table(mut job_table) = item {
                let job_id = match job_table.remove("id") {
                    Some(toml::Value::String(s)) if !s.trim().is_empty() => s,
                    _ => format!("job-{idx}"),
                };
                if !new_cron_map.contains_key(&job_id) {
                    new_cron_map.insert(job_id, toml::Value::Table(job_table));
                }
            }
        }
    }

    if !new_cron_map.is_empty() {
        table.insert("cron".to_string(), toml::Value::Table(new_cron_map));
    }
}

// ── File-level migration (comment-preserving) ───────────────────────────────
//
// Uses `migrate_to_current` to compute the migrated Config, then syncs the
// original toml_edit document to match. The sync function is generic — it
// doesn't know field names, it just diffs two table structures.

/// Migrate a raw TOML config file, preserving comments and formatting.
/// Returns `None` if already at current version with no legacy keys to fold.
pub fn migrate_file(raw: &str) -> Result<Option<String>> {
    // Short-circuit when the input is already at the current schema version
    // and carries no V1 top-level keys we'd need to fold. Comparing against
    // the parsed table (not a full Config deserialize) keeps this cheap.
    let probe: toml::Table = toml::from_str(raw).context("Failed to parse config table")?;
    let already_current = probe
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .is_some_and(|v| v as u32 >= CURRENT_SCHEMA_VERSION)
        && !V1_LEGACY_KEYS.iter().any(|k| probe.contains_key(*k));
    if already_current {
        return Ok(None);
    }

    let config = migrate_to_current(raw).context("Failed to migrate config")?;

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
