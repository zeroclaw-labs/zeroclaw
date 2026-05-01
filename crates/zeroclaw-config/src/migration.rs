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
//! 2. If the legacy field was on `V1Compat`, update `migrate_providers()` (or the
//!    relevant `V1Compat` method) to move the value into the new location.
//! 3. For changes between V2+ layouts, add a `fn vN_to_vM(&mut Config)` and call
//!    it from `into_config()` after the schema-version check.
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
    /// Consume self, migrating old fields into the current Config layout.
    pub fn into_config(mut self) -> super::schema::Config {
        let from = self.config.schema_version;
        let needs_migration = from < CURRENT_SCHEMA_VERSION || self.has_legacy_fields();

        if !needs_migration {
            return self.config;
        }

        self.migrate_providers();
        self.config.schema_version = CURRENT_SCHEMA_VERSION;

        tracing::info!(
            from = from,
            to = CURRENT_SCHEMA_VERSION,
            "Config schema migrated in-memory from version {from} to {CURRENT_SCHEMA_VERSION}. \
             Run `zeroclaw config migrate` to update the file on disk.",
        );

        self.config
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

    fn migrate_providers(&mut self) {
        let fallback = self
            .default_provider
            .take()
            .unwrap_or_else(|| "default".into());

        // First, move old model_providers entries into providers.models.
        // These take precedence over top-level fields (more specific).
        for (key, profile) in std::mem::take(&mut self.model_providers) {
            self.config.providers.models.entry(key).or_insert(profile);
        }

        // Then fill gaps in the fallback entry from top-level fields.
        let entry = self
            .config
            .providers
            .models
            .entry(fallback.clone())
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

        if self.config.providers.fallback.is_none() {
            self.config.providers.fallback = Some(fallback);
        }

        // Move routing rules into providers.
        if self.config.providers.model_routes.is_empty() && !self.model_routes.is_empty() {
            self.config.providers.model_routes = std::mem::take(&mut self.model_routes);
        }
        if self.config.providers.embedding_routes.is_empty() && !self.embedding_routes.is_empty() {
            self.config.providers.embedding_routes = std::mem::take(&mut self.embedding_routes);
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

/// Pre-deserialization table migration for nested field changes that
/// `#[serde(flatten)]` cannot capture (e.g. removing a field from a nested
/// struct and moving its value elsewhere).
///
/// Called on the raw `toml::Table` before it is deserialized into `V1Compat`.
pub fn prepare_table(table: &mut toml::Table) {
    // Migrate channels_config.matrix.room_id → channels_config.matrix.allowed_rooms
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(matrix)) = channels.get_mut("matrix")
        {
            scalar_to_vec(matrix, "room_id", "allowed_rooms");
        }
    }

    // Migrate channels.slack.channel_id → channels.slack.channel_ids
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(slack)) = channels.get_mut("slack")
        {
            scalar_to_vec(slack, "channel_id", "channel_ids");
        }
    }

    // V3: Migrate channels.mattermost.channel_id → channels.mattermost.channel_ids
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(mattermost)) = channels.get_mut("mattermost")
        {
            scalar_to_vec(mattermost, "channel_id", "channel_ids");
        }
    }

    // V3: Migrate channels.discord.guild_id → channels.discord.guild_ids
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(discord)) = channels.get_mut("discord")
        {
            scalar_to_vec(discord, "guild_id", "guild_ids");
        }
    }

    // V3: Migrate channels.signal.group_id → channels.signal.{group_ids, dm_only}
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

    // V3: Migrate channels.reddit.subreddit → channels.reddit.subreddits
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key)
            && let Some(toml::Value::Table(reddit)) = channels.get_mut("reddit")
        {
            scalar_to_vec(reddit, "subreddit", "subreddits");
        }
    }

    // V3: Fold [channels.discord-history] into [channels.discord].
    // discord-history was a separate channel type that archived ALL messages.
    // V3 merges it into discord with `archive = true`.
    for key in &["channels_config", "channels"] {
        if let Some(toml::Value::Table(channels)) = table.get_mut(*key) {
            // Pull the discord-history block out (try both hyphen and underscore).
            let dh = channels
                .remove("discord-history")
                .or_else(|| channels.remove("discord_history"));
            if let Some(toml::Value::Table(mut dh_table)) = dh {
                // Migrate discord-history's own guild_id to guild_ids.
                scalar_to_vec(&mut dh_table, "guild_id", "guild_ids");
                // Drop fields that no longer exist on discord config.
                dh_table.remove("store_dms");
                dh_table.remove("respond_to_dms");

                if let Some(toml::Value::Table(discord)) = channels.get_mut("discord") {
                    // Both blocks present: fold archive flag + channel_ids into discord.
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
                            "v2→v3 migration: [channels.discord-history] has a different \
                             bot_token than [channels.discord]. Discarding discord-history \
                             config; re-configure archive manually under [channels.discord]."
                        );
                    } else {
                        discord.insert("archive".to_string(), toml::Value::Boolean(true));
                        // Merge channel_ids from discord-history if discord has none.
                        if let Some(dh_ids) = dh_table.remove("channel_ids")
                            && discord.get("channel_ids").is_none()
                        {
                            discord.insert("channel_ids".to_string(), dh_ids);
                        }
                    }
                } else {
                    // Only discord-history exists: promote it to discord with archive=true.
                    dh_table.insert("archive".to_string(), toml::Value::Boolean(true));
                    channels.insert("discord".to_string(), toml::Value::Table(dh_table));
                }
            }
        }
    }

    // Rename legacy `channels_config` key to `channels`
    if table.contains_key("channels_config")
        && !table.contains_key("channels")
        && let Some(val) = table.remove("channels_config")
    {
        table.insert("channels".to_string(), val);
    }

    // V3: Drop the global `[cost.prices.*]` table. Pricing now lives on each
    // `[providers.models.<provider>.<alias>]` block. The global-hash key
    // (`"<provider>/<model>"`) does not carry the user's alias path, so no
    // automatic remapping is attempted; emit one INFO log per dropped entry
    // so operators can paste the values under the correct alias.
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

    // V3: Migrate legacy top-level [memory] pgvector fields to [memory.postgres].
    // PR #4714 removed the Postgres backend, stripping pgvector_enabled,
    // pgvector_dimensions, and db_url from [memory]. PR #6015 re-introduced
    // them under [memory.postgres]. Configs that still carry the old top-level
    // keys are moved here so nothing is silently dropped.
    if let Some(toml::Value::Table(memory)) = table.get_mut("memory") {
        let pg_enabled = memory.remove("pgvector_enabled");
        let pg_dims = memory.remove("pgvector_dimensions");
        // db_url moved to [storage]; we only drop it here to avoid an
        // unknown-key warning — operators should set it under [storage].
        let _ = memory.remove("db_url");

        if pg_enabled.is_some() || pg_dims.is_some() {
            let postgres = memory
                .entry("postgres")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(pg) = postgres {
                if let Some(v) = pg_enabled {
                    pg.entry("vector_enabled").or_insert(v);
                }
                if let Some(v) = pg_dims {
                    pg.entry("vector_dimensions").or_insert(v);
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
