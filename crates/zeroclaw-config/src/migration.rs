use anyhow::{Context, Result};
use std::path::Path;

use crate::schema::Config;
use crate::schema::v1::V1Config;
use crate::schema::v2::V2Config;

/// The schema version this binary writes and expects on disk.
///
/// V4 retires keys listed in [`RETIRED_KEYS`] (first: `[security.nevis]`).
pub const CURRENT_SCHEMA_VERSION: u32 = 4;

/// Something a migration changed or assumed about the operator's config.
///
/// Migrations report these instead of discarding or rewriting anything
/// silently. They carry key paths and reasons only, never values: a retired
/// table may hold a plaintext secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MigrationNotice {
    /// The file has no `schema_version`, so it was read as V1 and migrated
    /// from there. The V1 migration folds channel sections into a `default`
    /// alias, so a newer config missing only this key loses its aliases.
    AssumedV1,
    /// The file has no `schema_version`, but its sections are plainly in the
    /// V3 shape, so it was read as V3 and migrated from there (see
    /// [`INFERRED_SCHEMA_VERSION`]). Nothing was reshaped; the key is missing
    /// and should be added.
    InferredV3,
    /// A retired key was removed.
    Removed { path: String, reason: &'static str },
    /// An entry naming a retired channel was removed from the list at `path`
    /// (for example `agents.<alias>.channels`). `reference` is the channel
    /// reference itself, which names a channel and holds no secret.
    ReferenceRemoved {
        path: String,
        reference: String,
        reason: &'static str,
    },
    /// An entry naming a retired channel was left in place, because removing
    /// it would change more than the reference: the entry at `path` is either
    /// a peer group bound to the channel (a hard reference that is never
    /// deleted for the operator) or the last channel binding any agent has,
    /// whose removal would route every channel to the fallback agent. The
    /// operator must fix it; `Config::validate` reports it until then.
    ReferenceKept {
        path: String,
        reference: String,
        reason: &'static str,
    },
    /// A `ZEROCLAW_*` environment variable names a retired key, so it was
    /// ignored instead of failing startup.
    IgnoredEnvOverride {
        variable: String,
        reason: &'static str,
    },
    /// A retired key was moved to its replacement.
    Renamed {
        from: String,
        to: String,
        reason: &'static str,
    },
    /// A retired key was removed without being moved, because its
    /// replacement was already set (the replacement wins).
    RenameConflict {
        from: String,
        to: String,
        reason: &'static str,
    },
}

impl MigrationNotice {
    /// Whether the notice reports a change to the config file, as opposed to
    /// something left as it is for the operator to fix: a kept reference, or
    /// an ignored environment variable. Only changing notices make a
    /// migration write the file.
    #[must_use]
    pub fn changes_file(&self) -> bool {
        !matches!(
            self,
            Self::ReferenceKept { .. } | Self::IgnoredEnvOverride { .. }
        )
    }

    /// One-line English description, for logs and as the fallback for
    /// localized CLI output.
    pub fn message(&self) -> String {
        match self {
            Self::AssumedV1 => "config has no `schema_version`, so it was read as schema V1 and \
                 migrated from there. The V1 migration reshapes sections written for a \
                 newer version: provider entries can end up nested one level too deep, and \
                 channel sections are merged into a `default` alias. If this file was \
                 written for a newer ZeroClaw, add `schema_version` at the top with the \
                 version it was written for, and restore any lost providers or channel \
                 aliases."
                .to_string(),
            Self::InferredV3 => "config has no `schema_version`, but its sections are in the V3 \
                 format, so it was read as schema V3 and migrated from there. Run `zeroclaw \
                 config migrate` to write the current `schema_version` into the file."
                .to_string(),
            Self::Removed { path, reason } => {
                format!("removed retired config key `{path}`: {reason}")
            }
            Self::ReferenceRemoved {
                path,
                reference,
                reason,
            } => format!("removed `{reference}` from `{path}`: {reason}"),
            Self::ReferenceKept {
                path,
                reference,
                reason,
            } => {
                format!("kept `{reference}` in `{path}` although its channel is retired: {reason}")
            }
            Self::IgnoredEnvOverride { variable, reason } => {
                format!("ignored `{variable}`, which sets a retired config key: {reason}")
            }
            Self::Renamed { from, to, reason } => {
                format!("moved retired config key `{from}` to `{to}`: {reason}")
            }
            Self::RenameConflict { from, to, reason } => format!(
                "removed retired config key `{from}` without moving it, because `{to}` \
                 is already set: {reason}"
            ),
        }
    }
}

/// How a retired key is carried into the schema version that retires it.
#[derive(Debug, Clone, Copy)]
pub enum Retirement {
    /// Delete the key (and everything under it).
    Remove,
    /// Move the key's value to `to`, unless `to` is already set.
    Rename { to: &'static [&'static str] },
    /// Retire a channel type: delete `[channels.<type>]` like [`Self::Remove`]
    /// and every reference to it, so nothing is left naming a channel that no
    /// longer exists. That is each `[agents.<alias>] channels` entry of the
    /// type, and each `[peer_groups.<name>]` whose `channel` is of the type.
    /// The path must be `["channels", "<type>"]`. References are pruned even
    /// when the section itself is already gone.
    RemoveChannel,
}

/// A [`RetiredKey::path`] segment that matches every key of the table at that
/// level, e.g. `&["agents", ANY_KEY, "max_tool_iterations"]` retires the key in
/// every `[agents.<alias>]` block. In a [`Retirement::Rename`] target, each
/// `ANY_KEY` is filled with the key the matching source wildcard matched, in
/// order.
pub const ANY_KEY: &str = "*";

/// One retired config key.
#[derive(Debug, Clone, Copy)]
pub struct RetiredKey {
    /// The schema version whose migration step retires the key.
    pub retired_in: u32,
    /// Path from the config root, one segment per table key. [`ANY_KEY`]
    /// matches every key at its level.
    pub path: &'static [&'static str],
    pub retirement: Retirement,
    /// Why it was retired and what to use instead. Shown to the operator.
    pub reason: &'static str,
}

/// Every retired key, applied by the migration chain when it reaches
/// `retired_in`, and again on every load of a current config so a key that
/// reappears is dropped with a notice rather than silently ignored. Retiring
/// another key is one entry here plus removing its schema field, and a version
/// bump (a new `MIGRATION_STEPS` entry) if no pending version already covers it.
pub const RETIRED_KEYS: &[RetiredKey] = &[
    RetiredKey {
        retired_in: 4,
        path: &["security", "nevis"],
        retirement: Retirement::Remove,
        reason: "the Nevis IAM integration was removed; configure `[oidc.<alias>]` with \
                 `[users]` and `[permission_profiles]` instead",
    },
    // Agent-inline runtime tunables: superseded by runtime profiles and never
    // read from `[agents.<alias>]`, so serde ignored them silently.
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "compact_context"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "max_tool_iterations"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "max_history_messages"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "max_context_tokens"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "memory_recall_limit"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "parallel_tools"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "tool_dispatcher"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "strict_tool_parsing"],
        retirement: Retirement::Remove,
        reason: INERT_AGENT_TUNABLE,
    },
    RetiredKey {
        retired_in: 4,
        path: &[
            "runtime_profiles",
            ANY_KEY,
            "context_compression",
            "summary_model",
        ],
        retirement: Retirement::Remove,
        reason: "the deprecated bare model id had no provider identity and was never read; \
                 `summary_provider` replaced it (context compression itself is not \
                 currently implemented in the runtime)",
    },
    // Retired before V4 and until now handled outside this table: the
    // migration chain stripped `[node_transport]` silently after every step
    // from V2 on, and load only warned about WATI sections. Under V4 they
    // follow the same policy, so every load, migration and incremental save
    // removes them, secrets included.
    RetiredKey {
        retired_in: 2,
        path: &["node_transport"],
        retirement: Retirement::Remove,
        reason: "the legacy HMAC node transport was removed",
    },
    RetiredKey {
        retired_in: 4,
        path: &["channels", "wati"],
        retirement: Retirement::Remove,
        reason: RETIRED_WATI,
    },
    RetiredKey {
        retired_in: 4,
        path: &["channels_config", "wati"],
        retirement: Retirement::Remove,
        reason: RETIRED_WATI,
    },
    // Spellings no schema field reads, so serde would ignore them silently.
    // Twitter and Reddit stay channels (`[channels.twitter.<alias>]`,
    // `[channels.reddit.<alias>]`) and Notion stays the top-level `[notion]`
    // tool section; only these other spellings are dropped.
    RetiredKey {
        retired_in: 4,
        path: &["twitter"],
        retirement: Retirement::Remove,
        reason: "a top-level `[twitter]` section is not read; configure the Twitter channel as \
                 `[channels.twitter.<alias>]`",
    },
    RetiredKey {
        retired_in: 4,
        path: &["reddit"],
        retirement: Retirement::Remove,
        reason: "a top-level `[reddit]` section is not read; configure the Reddit channel as \
                 `[channels.reddit.<alias>]`",
    },
    RetiredKey {
        retired_in: 4,
        path: &["channels", "notion"],
        retirement: Retirement::RemoveChannel,
        reason: "Notion is not a channel, so `[channels.notion]` and references to a `notion` \
                 channel are not read; the Notion tool is configured in the top-level `[notion]` \
                 section",
    },
    // Retired by the V2 -> V3 step, whose normalizer also surfaces the shared
    // `[gateway.pairing_code]` policy. This entry covers files already at V3
    // or later, which never pass through that step.
    RetiredKey {
        retired_in: 3,
        path: &["gateway", "pairing_dashboard", "code_length"],
        retirement: Retirement::Remove,
        reason: "the dashboard no longer has its own pairing-code length; \
                 `[gateway.pairing_code]` `length` sets it for every pairing surface",
    },
    // Also dropped by the V2 -> V3 step, which moves or discards them while
    // restructuring. No current field reads them, so a V3 or later file that
    // still holds one would otherwise keep it forever.
    RetiredKey {
        retired_in: 3,
        path: &["swarms"],
        retirement: Retirement::Remove,
        reason: "swarms were removed in V3",
    },
    RetiredKey {
        retired_in: 3,
        path: &["reliability", "fallback_providers"],
        retirement: Retirement::Remove,
        reason: RETIRED_GLOBAL_FALLBACK,
    },
    RetiredKey {
        retired_in: 3,
        path: &["reliability", "model_fallbacks"],
        retirement: Retirement::Remove,
        reason: RETIRED_GLOBAL_FALLBACK,
    },
    RetiredKey {
        retired_in: 3,
        path: &["tts", "default_provider"],
        retirement: Retirement::Remove,
        reason: "there is no global default TTS provider; set `tts_provider` on each \
                 `[agents.<alias>]` instead",
    },
    RetiredKey {
        retired_in: 3,
        path: &["transcription", "default_provider"],
        retirement: Retirement::Remove,
        reason: RETIRED_GLOBAL_TRANSCRIPTION,
    },
    RetiredKey {
        retired_in: 3,
        path: &["transcription", "default_model_provider"],
        retirement: Retirement::Remove,
        reason: RETIRED_GLOBAL_TRANSCRIPTION,
    },
    RetiredKey {
        retired_in: 3,
        path: &["transcription", "default_transcription_provider"],
        retirement: Retirement::Remove,
        reason: RETIRED_GLOBAL_TRANSCRIPTION,
    },
    RetiredKey {
        retired_in: 3,
        path: &["identity"],
        retirement: Retirement::Remove,
        reason: "identity is set per agent; move it to `[agents.<alias>.identity]`",
    },
    // The rest of the dashboard table, after the V3 entry for its
    // `code_length`. Pairing never read these settings: its limits are fixed
    // in the pairing guard.
    RetiredKey {
        retired_in: 4,
        path: &["gateway", "pairing_dashboard"],
        retirement: Retirement::Remove,
        reason: "never read: pairing uses fixed limits (5 failed attempts, 300 s lockout, \
                 10-minute code lifetime); `[gateway.pairing_code]` sets the code shape",
    },
];

/// The retired key a config path lies at or under, matching [`ANY_KEY`]
/// segments against any key. Used where a path arrives from outside the
/// file, such as an environment override, to recognize a retired key rather
/// than fail on it as unknown.
#[must_use]
pub fn retired_key_covering(path: &[&str]) -> Option<&'static RetiredKey> {
    RETIRED_KEYS.iter().find(|key| {
        key.path.len() <= path.len()
            && key
                .path
                .iter()
                .zip(path)
                .all(|(pattern, segment)| *pattern == ANY_KEY || pattern == segment)
    })
}

const RETIRED_GLOBAL_FALLBACK: &str = "the global fallback lists were removed in V3; set \
     `fallback` on each `[providers.models.<type>.<alias>]` instead";

const RETIRED_GLOBAL_TRANSCRIPTION: &str = "there is no global default transcription provider; \
     set `transcription_provider` on each `[agents.<alias>]` instead";

const RETIRED_WATI: &str = "WATI support was removed; migrate to `[channels.whatsapp.<alias>]` \
     using the Cloud API or WhatsApp Web, then revoke the unused WATI API token";

const INERT_AGENT_TUNABLE: &str = "agent-inline runtime tunables were never read; runtime \
     profiles are authoritative, so set this key on the agent's \
     `[runtime_profiles.<profile>]` instead";

pub(crate) struct ConfigLoadAttribution;

impl zeroclaw_api::attribution::Attributable for ConfigLoadAttribution {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::System
    }
    fn alias(&self) -> &str {
        "config"
    }
}

pub const V1_LEGACY_KEYS: &[&str] = &[
    "api_key",
    "api_url",
    "api_path",
    "default_model_provider",
    "default_model",
    "model_providers",
    "default_temperature",
    "provider_timeout_secs",
    "provider_max_tokens",
    "extra_headers",
    "model_routes",
    "embedding_routes",
    "channels_config",
    "autonomy",
    "agent",
    "swarms",
    "cron",
];

/// The version a config with no `schema_version` key is read as when its
/// sections are plainly in the V3 shape: at least one alias-keyed
/// `providers.models.<family>` or `channels.<type>` section, none holding
/// fields directly (the V2 shape), and no V1-only top-level key.
///
/// Fixed at 3, not [`CURRENT_SCHEMA_VERSION`]: V3 is the newest shape such a
/// file can be recognized by, and reading it as V3 runs every later migration
/// step on it. Reading it as the current version would skip them.
pub const INFERRED_SCHEMA_VERSION: u32 = 3;

pub fn detect_version(value: &toml::Value) -> Result<u32> {
    let table = value
        .as_table()
        .context("config root must be a TOML table")?;
    match table.get("schema_version") {
        None => match unversioned_shape(table) {
            UnversionedShape::V3 => Ok(INFERRED_SCHEMA_VERSION),
            UnversionedShape::NotV3 => Ok(1),
            UnversionedShape::Ambiguous { detail } => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "detail": detail })),
                    "config has no schema_version and its shape is ambiguous"
                );
                anyhow::bail!(
                    "config has no `schema_version`, and {detail}. Add `schema_version = 2` or \
                     `schema_version = 3` as the first line of the file so it is not guessed"
                )
            }
        },
        Some(toml::Value::Integer(n)) if *n >= 1 => u32::try_from(*n).map_err(|_| {
            anyhow::Error::msg(format!(
                "config schema_version {n} is newer than this binary supports \
                 ({CURRENT_SCHEMA_VERSION})"
            ))
        }),
        Some(other) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"found": other.to_string()})),
                "config schema_version is not a positive integer"
            );
            anyhow::bail!("schema_version must be a positive integer, got {other}")
        }
    }
}

/// [`V1_LEGACY_KEYS`] that are also top-level sections of the current schema
/// (with a different shape), so their presence says nothing about the
/// version. Kept in step with the schema by
/// `v1_keys_still_current_are_exactly_the_current_top_level_sections`.
const V1_KEYS_STILL_CURRENT: &[&str] = &["model_routes", "embedding_routes", "cron"];

/// How a config with no `schema_version` key reads, by its shape.
enum UnversionedShape {
    /// Unmistakably the V3 shape: read it as [`INFERRED_SCHEMA_VERSION`].
    V3,
    /// Not the V3 shape: keep the historical V1 reading.
    NotV3,
    /// The V3 shape, but something in it also reads as V2: a child table
    /// that is a field of a V2 entry, or a key only V2 has. Either reading
    /// can lose configuration, so neither is chosen and the operator must
    /// state the version. `detail` says what reads both ways.
    Ambiguous { detail: String },
}

/// Whether a config with no `schema_version` key is unmistakably written in
/// the V3 shape.
///
/// A missing key historically meant V1, since V1 files predate the key. But a
/// hand-written or template-generated V3 file can omit it too, and running
/// such a file through the V1 migration silently destroys it: alias-keyed
/// channel sections collapse into `default` and the other aliases are
/// dropped. So a missing key is read as V3 when the file carries the V3 shape
/// and nothing older:
///
/// - no V1-only top-level key ([`V1_LEGACY_KEYS`] minus the keys still
///   current);
/// - at least one alias-keyed section, `providers.models.<family>` or
///   `channels.<type>`, meaning a table whose every value is a table
///   (`[providers.models.ollama.default]`, `[channels.discord.work]`);
/// - no such section holding fields directly, which is the V2 shape
///   (`[providers.models.ollama] model = "..."`, `[channels.discord]
///   bot_token = "..."`).
///
/// A V2 entry whose only content is map-valued fields has the alias-keyed
/// shape too: `[providers.models.openai.extra_headers]` is either the V2
/// `openai` profile's headers or a V3 alias named `extra_headers`. When a
/// child table of an alias-keyed section also reads as a field of a V2 entry
/// (see [`keys_read_as_v2_entry_fields`]), the shape is ambiguous.
///
/// Anything else keeps the V1 reading. A V4 file has the same shape, so an
/// unversioned V4 file is read as V3 too; its V3 -> V4 step changes nothing
/// it has not already done.
fn unversioned_shape(table: &toml::Table) -> UnversionedShape {
    if V1_LEGACY_KEYS
        .iter()
        .filter(|key| !V1_KEYS_STILL_CURRENT.contains(key))
        .any(|key| table.contains_key(*key))
    {
        return UnversionedShape::NotV3;
    }

    let provider_families = table
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get("models"))
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|families| {
            families
                .iter()
                .map(|(family, section)| (V2EntryKind::ModelProvider, family.as_str(), section))
        });
    let channel_types = table
        .get("channels")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|channels| {
            crate::schema::v2::V3_CHANNEL_TYPES
                .iter()
                .filter_map(|kind| {
                    channels
                        .get(*kind)
                        .map(|section| (V2EntryKind::Channel, *kind, section))
                })
        });

    let mut alias_keyed = false;
    let mut ambiguous = None;
    for (entry_kind, name, section) in provider_families.chain(channel_types) {
        let Some(section) = section.as_table() else {
            continue;
        };
        if section.is_empty() {
            continue;
        }
        if !section.values().all(toml::Value::is_table) {
            // A field held directly on the section: the V2 shape.
            return UnversionedShape::NotV3;
        }
        alias_keyed = true;
        if ambiguous.is_none()
            && let Some(key) = keys_read_as_v2_entry_fields(entry_kind, name, section)
                .into_iter()
                .next()
        {
            let section = format!("{}.{name}", entry_kind.path());
            ambiguous = Some(format!(
                "`[{section}.{key}]` reads either as the `{key}` field of a V2 `{section}` \
                 entry or as a V3 alias named `{key}`"
            ));
        }
    }
    if !alias_keyed {
        return UnversionedShape::NotV3;
    }
    if let Some(detail) = ambiguous.or_else(|| v2_only_marker(table)) {
        return UnversionedShape::Ambiguous { detail };
    }
    UnversionedShape::V3
}

/// Keys the V2 -> V3 step moves off `[agents.<alias>]` (see
/// `schema::v2::synthesize_agent_brains`) that no V3 agent has. The inert
/// tunables a V3 file may still carry, such as `max_tool_iterations`, are
/// not listed: they say nothing about the version. Kept honest by
/// `v2_only_agent_keys_are_not_v3_agent_fields`.
const V2_ONLY_AGENT_KEYS: &[&str] = &[
    "provider",
    "model",
    "api_key",
    "temperature",
    "max_iterations",
    "allowed_tools",
    "agentic",
    "max_depth",
    "skills_directory",
    "memory_namespace",
    "agentic_timeout_secs",
    "timeout_secs",
];

/// A setting only V2 reads, found in a file whose other sections are in the
/// V3 shape: a value held directly on `[providers]` (V3 keeps only the
/// `models`, `tts` and `transcription` tables there) or a V2-only agent key.
/// Read as V3 it would be ignored; read as V1 the V3 sections would be
/// reshaped. `None` when there is no such key.
fn v2_only_marker(table: &toml::Table) -> Option<String> {
    if let Some(providers) = table.get("providers").and_then(toml::Value::as_table)
        && let Some((key, _)) = providers.iter().find(|(_, value)| !value.is_table())
    {
        return Some(format!(
            "`providers.{key}` is a V2 setting the V3 layout does not read, while other \
             sections are in the V3 layout"
        ));
    }
    let agents = table.get("agents").and_then(toml::Value::as_table)?;
    agents.iter().find_map(|(alias, agent)| {
        let agent = agent.as_table()?;
        let key = V2_ONLY_AGENT_KEYS
            .iter()
            .find(|key| agent.contains_key(**key))?;
        Some(format!(
            "`agents.{alias}.{key}` is a V2 setting the V3 layout does not read, while other \
             sections are in the V3 layout"
        ))
    })
}

/// The kind of entry an alias-keyed section holds.
#[derive(Clone, Copy)]
enum V2EntryKind {
    ModelProvider,
    Channel,
}

impl V2EntryKind {
    fn path(self) -> &'static str {
        match self {
            Self::ModelProvider => "providers.models",
            Self::Channel => "channels",
        }
    }
}

/// The child keys of `section` (the `name` family or channel type) that also
/// read as fields when the section is taken as one V2 entry rather than a
/// table of V3 aliases.
///
/// Answered by the schema itself rather than a hand-kept list: the section is
/// deserialized as a single entry of its real type and serialized back, and a
/// child key that survives is a field of that entry. A key whose table does
/// not fit the field's type, or a section that does not read as a single
/// entry at all, does not survive, so only a genuine second reading counts.
fn keys_read_as_v2_entry_fields(
    kind: V2EntryKind,
    name: &str,
    section: &toml::Table,
) -> Vec<String> {
    let survivors = round_trip_as_v2_entry(kind, name, section).unwrap_or_default();
    section
        .keys()
        .filter(|key| {
            survivors.contains_key(key.as_str()) || survives_with_a_filler(kind, name, section, key)
        })
        .cloned()
        .collect()
}

/// Whether `key` is a map-valued field whose own content was dropped from the
/// round trip: an empty map, or one whose values all equal their defaults, is
/// skipped on serialization, so `[providers.models.openai.extra_headers]` with
/// no headers would not survive as itself. The child is replaced by a
/// one-entry map of each value shape a field map can hold; if any of them
/// survives, the key is a field.
fn survives_with_a_filler(kind: V2EntryKind, name: &str, section: &toml::Table, key: &str) -> bool {
    const FILLER_KEY: &str = "__unversioned_shape_filler__";
    let fillers = [
        toml::Value::String("filler".to_string()),
        toml::Value::Float(1.5),
        toml::Value::Integer(1),
        toml::Value::Boolean(true),
        toml::Value::Table(toml::Table::new()),
    ];
    fillers.into_iter().any(|filler| {
        let mut probe = section.clone();
        let mut child = toml::Table::new();
        child.insert(FILLER_KEY.to_string(), filler);
        probe.insert(key.to_string(), toml::Value::Table(child));
        round_trip_as_v2_entry(kind, name, &probe).is_some_and(|entry| entry.contains_key(key))
    })
}

/// `section` deserialized as one entry of its real type and serialized back,
/// or `None` when it does not read as a single entry at all.
fn round_trip_as_v2_entry(
    kind: V2EntryKind,
    name: &str,
    section: &toml::Table,
) -> Option<toml::Table> {
    const PROBE: &str = "__unversioned_shape_probe__";
    let mut slot = toml::Table::new();
    slot.insert(PROBE.to_string(), toml::Value::Table(section.clone()));
    let mut wrapped = toml::Table::new();
    wrapped.insert(name.to_string(), toml::Value::Table(slot));
    let wrapped = toml::Value::Table(wrapped);
    let round_tripped = match kind {
        V2EntryKind::ModelProvider => wrapped
            .try_into::<crate::providers::ModelProviders>()
            .ok()
            .and_then(|parsed| toml::Value::try_from(parsed).ok()),
        V2EntryKind::Channel => wrapped
            .try_into::<crate::schema::ChannelsConfig>()
            .ok()
            .and_then(|parsed| toml::Value::try_from(parsed).ok()),
    };
    round_tripped
        .as_ref()
        .and_then(|value| value.get(name))
        .and_then(|slot| slot.get(PROBE))
        .and_then(toml::Value::as_table)
        .cloned()
}

/// The notice for a config with no `schema_version` key, by the version it
/// was read as: V1 by default, V3 when its shape leaves no doubt. `None` when
/// the key is present.
fn unversioned_notice(value: &toml::Value, detected: u32) -> Option<MigrationNotice> {
    let unversioned = value
        .as_table()
        .is_some_and(|root| !root.contains_key("schema_version"));
    match (unversioned, detected) {
        (false, _) => None,
        (true, INFERRED_SCHEMA_VERSION) => Some(MigrationNotice::InferredV3),
        (true, _) => Some(MigrationNotice::AssumedV1),
    }
}

/// A parsed config carried to the current schema version, with what changed.
struct Migrated {
    value: toml::Value,
    notices: Vec<MigrationNotice>,
}

/// Carry a parsed config to [`CURRENT_SCHEMA_VERSION`]. `Ok(None)` when it is
/// already current and holds no retired key. Every notice is also logged at
/// WARN; callers that talk to an operator must surface them too, since WARN is
/// hidden without `-v`.
fn migrate_toml(value: toml::Value) -> Result<Option<Migrated>> {
    let from = detect_version(&value)?;
    if from == CURRENT_SCHEMA_VERSION {
        // A retired key can reappear in a current file (hand-edited, or
        // copied from an old example). The schema no longer has a field for
        // it, so serde would silently ignore it; drop it and say so instead.
        let mut value = value;
        let mut notices = Vec::new();
        for version in 2..=CURRENT_SCHEMA_VERSION {
            apply_retired_keys(&mut value, version, RETIRED_KEYS, &mut notices);
        }
        if !notices.iter().any(MigrationNotice::changes_file) {
            // Nothing to write. Notices about what was left for the operator
            // are reported by `Config::validate`, which flags each dangling
            // reference on every load.
            return Ok(None);
        }
        log_notices(&notices);
        return Ok(Some(Migrated { value, notices }));
    }
    if from > CURRENT_SCHEMA_VERSION {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "from_version": from,
                    "supported_version": CURRENT_SCHEMA_VERSION,
                })),
            "config schema_version is newer than this binary supports"
        );
        anyhow::bail!(
            "config schema_version {from} is newer than this binary supports ({CURRENT_SCHEMA_VERSION})"
        );
    }
    let mut notices: Vec<MigrationNotice> = unversioned_notice(&value, from).into_iter().collect();
    let value = run_chain(value, from, &mut notices)?;
    log_notices(&notices);
    Ok(Some(Migrated { value, notices }))
}

fn log_notices(notices: &[MigrationNotice]) {
    for notice in notices {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "notice": notice })),
            &notice.message()
        );
    }
}

pub fn migrate_file(input: &str) -> Result<Option<String>> {
    Ok(migrate_file_with_notices(input)?.map(|(migrated, _)| migrated))
}

/// The oldest schema version from which reaching [`CURRENT_SCHEMA_VERSION`]
/// changes a config only by retiring keys and re-stamping `schema_version`.
/// A file at or above it is migrated by editing its document in place, so
/// everything the migration does not change keeps its exact bytes: quoting,
/// number spelling, comments. Older files need the structural steps and are
/// rebuilt from the migrated value. Raise this if a later version adds a
/// structural migration step.
pub(crate) const FIRST_RETIREMENT_ONLY_VERSION: u32 = 3;

/// [`migrate_file`], also returning what the migration changed or assumed.
pub fn migrate_file_with_notices(input: &str) -> Result<Option<(String, Vec<MigrationNotice>)>> {
    let value: toml::Value = toml::from_str(input).context("failed to parse config TOML")?;
    let from = detect_version(&value)?;
    if (FIRST_RETIREMENT_ONLY_VERSION..=CURRENT_SCHEMA_VERSION).contains(&from)
        && let Ok(mut doc) = input.parse::<toml_edit::DocumentMut>()
    {
        let mut notices: Vec<MigrationNotice> =
            unversioned_notice(&value, from).into_iter().collect();
        notices.extend(apply_retired_keys_to_doc(doc.as_table_mut()));
        if from == CURRENT_SCHEMA_VERSION && !notices.iter().any(MigrationNotice::changes_file) {
            return Ok(None);
        }
        stamp_doc_schema_version(doc.as_table_mut());
        log_notices(&notices);
        return Ok(Some((doc.to_string(), notices)));
    }
    let Some(Migrated { value, notices }) = migrate_toml(value)? else {
        return Ok(None);
    };
    let migrated_table = match value {
        toml::Value::Table(t) => t,
        _ => {
            anyhow::bail!("migrated config is not a TOML table");
        }
    };

    // Try to preserve comments by reconciling into the original DocumentMut.
    // If the original doesn't parse as toml_edit (rare — toml::from_str
    // already succeeded on it), fall back to a fresh serialization.
    if let Ok(mut doc) = input.parse::<toml_edit::DocumentMut>() {
        sync_table(doc.as_table_mut(), &migrated_table);
        keep_emptied_tables_visible(doc.as_table_mut(), &notices);
        Ok(Some((doc.to_string(), notices)))
    } else {
        let serialized = toml::to_string_pretty(&toml::Value::Table(migrated_table))
            .context("failed to serialize migrated config")?;
        Ok(Some((serialized, notices)))
    }
}

/// Set `schema_version` to [`CURRENT_SCHEMA_VERSION`] in place, keeping the
/// key's position and its surrounding comments.
fn stamp_doc_schema_version(root: &mut toml_edit::Table) {
    let version = i64::from(CURRENT_SCHEMA_VERSION);
    if let Some(existing) = root
        .get_mut("schema_version")
        .and_then(toml_edit::Item::as_value_mut)
    {
        let decor = existing.decor().clone();
        let mut stamped = toml_edit::Value::from(version);
        *stamped.decor_mut() = decor;
        *existing = stamped;
    } else {
        root.insert("schema_version", toml_edit::value(version));
    }
}

/// Embedded V1 fixture used by [`generate`] / the `zeroclaw config generate`
/// CLI. Authored against the V1 schema at the parent of the V2-intro
/// commit; see `fixtures/v1.toml`.
const V1_FIXTURE: &str = include_str!("../fixtures/v1.toml");

/// Options for [`generate`].
#[derive(Debug, Default, Clone)]
pub struct GenerateOptions<'a> {
    /// Encrypt secret-bearing string values in the output. Works at every
    /// schema version via [`encrypt_secret_strings`], which walks the TOML
    /// and ChaCha20-Poly1305-encrypts any leaf whose key name appears in
    /// `SECRET_KEY_NAMES`.
    pub encrypt_secrets: bool,
    /// Directory containing (or to receive) the `.secret_key` used for
    /// `enc2:` encryption. Required when `encrypt_secrets` is true. The
    /// key is created with 0o600 permissions if absent — matches how the
    /// daemon's `SecretStore` behaves on first use.
    pub secret_store_dir: Option<&'a Path>,
}

pub fn generate(target_version: u32, opts: &GenerateOptions<'_>) -> Result<String> {
    if target_version == 0 || target_version > CURRENT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported schema version {target_version} \
             (valid: 1..={CURRENT_SCHEMA_VERSION})"
        );
    }

    let value = if target_version == 1 {
        toml::from_str::<toml::Value>(V1_FIXTURE).context("embedded V1 fixture is malformed")?
    } else {
        let v1_value: toml::Value =
            toml::from_str(V1_FIXTURE).context("embedded V1 fixture is malformed")?;
        run_chain_until(v1_value, 1, target_version, &mut Vec::new())?
    };

    let mut value = value;
    if opts.encrypt_secrets {
        let store_dir = opts.secret_store_dir.context(
            "--encrypt requires a secret-store directory \
             (typically the resolved ZEROCLAW_CONFIG_DIR)",
        )?;
        let store = crate::secrets::SecretStore::new(store_dir, true);
        encrypt_secret_strings(&mut value, &store)
            .context("failed to encrypt secret-bearing fields in generated config")?;
    }

    toml::to_string_pretty(&value).context("failed to serialize generated config")
}

fn secret_key_names() -> &'static std::collections::HashSet<&'static str> {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static CACHE: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CACHE.get_or_init(|| Config::secret_field_terminals().into_iter().collect())
}

pub fn encrypt_secret_strings(
    value: &mut toml::Value,
    store: &crate::secrets::SecretStore,
) -> Result<()> {
    let names = secret_key_names();
    encrypt_walk(value, store, names)
}

fn encrypt_walk(
    value: &mut toml::Value,
    store: &crate::secrets::SecretStore,
    names: &std::collections::HashSet<&'static str>,
) -> Result<()> {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table.iter_mut() {
                if names.contains(key.as_str()) {
                    encrypt_in_place(child, store)
                        .with_context(|| format!("encrypting secret at key `{key}`"))?;
                } else {
                    encrypt_walk(child, store, names)?;
                }
            }
        }
        toml::Value::Array(items) => {
            for item in items.iter_mut() {
                encrypt_walk(item, store, names)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn encrypt_in_place(value: &mut toml::Value, store: &crate::secrets::SecretStore) -> Result<()> {
    match value {
        toml::Value::String(s)
            if !crate::secrets::SecretStore::is_encrypted(s) && !s.is_empty() =>
        {
            let encrypted = store.encrypt(s).context("encrypt string")?;
            *s = encrypted;
        }
        toml::Value::Array(items) => {
            for item in items.iter_mut() {
                encrypt_in_place(item, store)?;
            }
        }
        toml::Value::Table(table) => {
            for (_, child) in table.iter_mut() {
                encrypt_in_place(child, store)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Versioned TOML → validated V3 `Config`, strict: any defect errors.
/// Used by repair tooling (`zeroclaw config migrate`, `model_routing_config`)
/// that needs the precise failure. Daemon load uses the resilient path.
pub fn migrate_to_current(input: &str) -> Result<Config> {
    let _attribution = ::zeroclaw_log::attribution_span!(&ConfigLoadAttribution).entered();
    let (final_value, _notices) = migrate_value(input)?;
    final_value
        .try_into()
        .context("migrated config failed to deserialize as current schema")
}

/// Daemon load path: versioned TOML → usable `Config`, never failing.
/// Thin wrapper over [`migrate_to_current_salvaged`] that drops the report.
pub fn migrate_to_current_resilient(input: &str) -> Config {
    migrate_to_current_salvaged(input).config
}

/// Top-level keys whose silent loss could *weaken* security posture: dropping
/// a malformed one to its `Default` may grant a broader posture than intended.
/// Salvage still drops them (so the daemon boots) but logs ERROR and reports
/// them in [`ResilientLoad::dropped_security`] for exposure gating.
pub const SECURITY_CRITICAL_KEYS: &[&str] = &[
    "security",
    "risk_profiles",
    "peer_groups",
    "users",
    "oidc",
    "permission_profiles",
];

pub const WHOLE_CONFIG_SENTINEL: &str = "<entire-config>";

/// Result of a resilient (never-failing) config load.
#[derive(Debug, Clone, Default)]
pub struct ResilientLoad {
    /// Loaded config: every section that parsed, `Default` for any dropped.
    pub config: Config,
    /// Non-security paths dropped during salvage (logged WARN).
    pub dropped: Vec<String>,
    /// [`SECURITY_CRITICAL_KEYS`] sections dropped to `Default` (logged ERROR).
    /// Non-empty means the running posture may be weaker than intended.
    pub dropped_security: Vec<String>,
    /// What migrating to the current schema changed or assumed.
    pub notices: Vec<MigrationNotice>,
}

pub fn migrate_to_current_salvaged(input: &str) -> ResilientLoad {
    let (value, notices) = match migrate_value(input) {
        Ok(migrated) => migrated,
        Err(err) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{err:#}") })),
                "config could not be parsed or migrated; starting on defaults so it \
                 can be repaired (gateway /api/config, `zeroclaw config migrate`)"
            );
            return ResilientLoad {
                config: Config::default(),
                dropped: Vec::new(),
                // Whole-config loss degrades the security posture: every
                // security-critical section is gone, so mark it so the serving
                // gate refuses to start without an explicit override.
                dropped_security: vec![WHOLE_CONFIG_SENTINEL.to_string()],
                notices: Vec::new(),
            };
        }
    };
    ResilientLoad {
        notices,
        ..deserialize_resilient(value)
    }
}

/// Parse + migrate to the current schema version as a `toml::Value`, without
/// the final typed deserialize. Shared by the strict and resilient entries.
fn migrate_value(input: &str) -> Result<(toml::Value, Vec<MigrationNotice>)> {
    let value: toml::Value = toml::from_str(input).context("failed to parse config TOML")?;
    match migrate_toml(value.clone())? {
        Some(Migrated { value, notices }) => Ok((value, notices)),
        None => Ok((value, Vec::new())),
    }
}

/// Deserialize a migrated `toml::Value` into `Config`, never failing.
/// Strict first; on failure prune broken channel aliases, channel types, then
/// top-level sections (each → `Default`), so only the broken blocks are lost.
fn deserialize_resilient(value: toml::Value) -> ResilientLoad {
    if let Ok(config) = value.clone().try_into::<Config>() {
        return ResilientLoad {
            config,
            dropped: Vec::new(),
            dropped_security: Vec::new(),
            notices: Vec::new(),
        };
    }

    let mut salvaged = value;
    let mut dropped: Vec<String> = Vec::new();
    prune_bad_channel_aliases(&mut salvaged, &mut dropped);
    prune_bad_channel_types(&mut salvaged, &mut dropped);
    prune_bad_provider_aliases(&mut salvaged, &mut dropped);
    prune_bad_top_level_sections(&mut salvaged, &mut dropped);

    let mut whole_config_lost = false;
    let config = salvaged.try_into::<Config>().unwrap_or_else(|err| {
        // Nothing in the root table is individually salvageable (e.g. a
        // non-table root). Boot on defaults so repair surfaces are reachable.
        whole_config_lost = true;
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({ "error": format!("{err:#}") })),
            "config could not be salvaged section-by-section; starting on defaults \
             so it can be repaired"
        );
        Config::default()
    });

    let mut dropped_security: Vec<String> = Vec::new();
    let mut dropped_plain: Vec<String> = Vec::new();
    // A whole-config default loses every security-critical section at once, so
    // mark it degraded even though no individual section was named in `dropped`.
    if whole_config_lost {
        dropped_security.push(WHOLE_CONFIG_SENTINEL.to_string());
    }
    for path in dropped {
        if SECURITY_CRITICAL_KEYS.contains(&path.as_str()) {
            dropped_security.push(path);
        } else {
            dropped_plain.push(path);
        }
    }

    for path in &dropped_plain {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "dropped_config": path })),
            &format!(
                "config section `{path}` is invalid and was skipped so the daemon can \
                 start; fix the block and reload to re-enable it"
            )
        );
    }
    for path in &dropped_security {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({ "dropped_security_config": path })),
            &format!(
                "SECURITY-CRITICAL config section `{path}` is invalid and was reset to \
                 its default so the daemon can boot; the running posture may be WEAKER \
                 than intended — repair `{path}` and reload before trusting this instance. \
                 Use the same executable that started this process with `config migrate` \
                 to see the precise parse error, or fix it via the gateway config editor \
                 at `/api/config`"
            )
        );
    }

    ResilientLoad {
        config,
        dropped: dropped_plain,
        dropped_security,
        notices: Vec::new(),
    }
}

/// Drop top-level `[section]`s that block deserialization (each → `Default`).
/// Two probes: drop a single key if its removal validates the whole config;
/// else drop every key that fails to deserialize in isolation (catches
/// multiple independent offenders the joint probe can't). Appends to `dropped`.
fn prune_bad_top_level_sections(value: &mut toml::Value, dropped: &mut Vec<String>) {
    if value.as_table().is_none() {
        return;
    }
    if value.clone().try_into::<Config>().is_ok() {
        return;
    }

    let keys: Vec<String> = value
        .as_table()
        .expect("root is a table")
        // toml::Value tables preserve insertion order, so drops are reported
        // in TOML declaration order — predictable for operators reading logs.
        .keys()
        .cloned()
        .collect();
    for key in &keys {
        let root = value.as_table_mut().expect("root is a table");
        let Some(removed) = root.remove(key) else {
            continue;
        };
        if value.clone().try_into::<Config>().is_ok() {
            dropped.push(key.clone());
            return;
        }
        value
            .as_table_mut()
            .expect("root is a table")
            .insert(key.clone(), removed);
    }

    for key in keys {
        let still_present = value.as_table().and_then(|root| root.get(&key)).cloned();
        let Some(section) = still_present else {
            continue;
        };
        if top_level_section_is_invalid(&key, &section) {
            value.as_table_mut().expect("root is a table").remove(&key);
            dropped.push(key);
        }
    }
}

/// True when top-level `[<key>]`, wrapped alone, fails to deserialize.
fn top_level_section_is_invalid(key: &str, section: &toml::Value) -> bool {
    let mut root = toml::value::Table::new();
    root.insert(key.to_string(), section.clone());
    toml::Value::Table(root).try_into::<Config>().is_err()
}

fn prune_bad_channel_aliases(value: &mut toml::Value, dropped: &mut Vec<String>) {
    let Some(channels) = value
        .as_table_mut()
        .and_then(|root| root.get_mut("channels"))
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };

    for (chan_type, aliases) in channels.iter_mut() {
        let Some(alias_table) = aliases.as_table_mut() else {
            continue;
        };
        let invalid: Vec<String> = alias_table
            .iter()
            .filter(|(_, v)| channel_alias_is_invalid(chan_type, v))
            .map(|(k, _)| k.clone())
            .collect();
        for alias in invalid {
            alias_table.remove(&alias);
            dropped.push(format!("channels.{chan_type}.{alias}"));
        }
    }
}

fn prune_bad_provider_aliases(value: &mut toml::Value, dropped: &mut Vec<String>) {
    let Some(provider_kinds) = value
        .as_table_mut()
        .and_then(|root| root.get_mut("providers"))
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };

    // Non-table nodes where a kind/family map is required (e.g.
    // `[providers.models] ollama = "oops"`) would otherwise still sink the
    // whole section in prune_bad_top_level_sections. Drop just the node.
    let scalar_kinds: Vec<String> = provider_kinds
        .iter()
        .filter(|(_, v)| !v.is_table())
        .map(|(k, _)| k.clone())
        .collect();
    for kind in scalar_kinds {
        provider_kinds.remove(&kind);
        dropped.push(format!("providers.{kind}"));
    }

    for (kind, families) in provider_kinds.iter_mut() {
        let family_table = families.as_table_mut().expect("scalar kinds pruned above");
        let scalar_families: Vec<String> = family_table
            .iter()
            .filter(|(_, v)| !v.is_table())
            .map(|(k, _)| k.clone())
            .collect();
        for family in scalar_families {
            family_table.remove(&family);
            dropped.push(format!("providers.{kind}.{family}"));
        }
        for (family, aliases) in family_table.iter_mut() {
            let alias_table = aliases
                .as_table_mut()
                .expect("scalar families pruned above");
            let invalid: Vec<String> = alias_table
                .iter()
                .filter(|(_, v)| provider_alias_is_invalid(kind, family, v))
                .map(|(k, _)| k.clone())
                .collect();
            for alias in invalid {
                alias_table.remove(&alias);
                dropped.push(format!("providers.{kind}.{family}.{alias}"));
            }
        }
    }
}

/// True when `[providers.<kind>.<family>.<alias>]`, wrapped alone, fails to
/// deserialize. Unknown families pass (serde ignores them); only a
/// known-family alias with bad field data is invalid.
fn provider_alias_is_invalid(kind: &str, family: &str, alias_value: &toml::Value) -> bool {
    let mut inner = toml::value::Table::new();
    inner.insert("probe".to_string(), alias_value.clone());
    let mut family_table = toml::value::Table::new();
    family_table.insert(family.to_string(), toml::Value::Table(inner));
    let mut kind_table = toml::value::Table::new();
    kind_table.insert(kind.to_string(), toml::Value::Table(family_table));
    let mut root = toml::value::Table::new();
    root.insert("providers".to_string(), toml::Value::Table(kind_table));
    toml::Value::Table(root).try_into::<Config>().is_err()
}

/// Drop each `[channels.<type>]` block still blocking the load after alias
/// pruning (e.g. a scalar where a table is required). Drops only the offending
/// type, never the whole `[channels]` section. Appends `channels.<type>`.
fn prune_bad_channel_types(value: &mut toml::Value, dropped: &mut Vec<String>) {
    let Some(channel_types) = value
        .as_table()
        .and_then(|root| root.get("channels"))
        .and_then(toml::Value::as_table)
        .map(|chans| chans.keys().cloned().collect::<Vec<_>>())
    else {
        return;
    };

    for chan_type in channel_types {
        if channels_section_is_valid(value) {
            return;
        }
        let Some(removed) = value
            .as_table_mut()
            .and_then(|root| root.get_mut("channels"))
            .and_then(toml::Value::as_table_mut)
            .and_then(|chans| chans.remove(&chan_type))
        else {
            continue;
        };
        if channels_section_is_valid(value) {
            dropped.push(format!("channels.{chan_type}"));
        } else {
            value
                .as_table_mut()
                .and_then(|root| root.get_mut("channels"))
                .and_then(toml::Value::as_table_mut)
                .expect("channels is a table")
                .insert(chan_type, removed);
        }
    }
}

/// True when `value`'s `[channels]` section deserializes cleanly in isolation.
fn channels_section_is_valid(value: &toml::Value) -> bool {
    let Some(channels) = value
        .as_table()
        .and_then(|root| root.get("channels"))
        .cloned()
    else {
        return true;
    };
    let mut root = toml::value::Table::new();
    root.insert("channels".to_string(), channels);
    toml::Value::Table(root).try_into::<Config>().is_ok()
}

/// True when `[channels.<type>.<alias>]`, wrapped alone, fails to deserialize.
fn channel_alias_is_invalid(chan_type: &str, alias_value: &toml::Value) -> bool {
    let mut inner = toml::value::Table::new();
    inner.insert("probe".to_string(), alias_value.clone());
    let mut type_table = toml::value::Table::new();
    type_table.insert(chan_type.to_string(), toml::Value::Table(inner));
    let mut channels = toml::value::Table::new();
    channels.insert("channels".to_string(), toml::Value::Table(type_table));
    toml::Value::Table(channels).try_into::<Config>().is_err()
}

pub fn migrate_file_in_place(path: &Path) -> Result<Option<MigrateReport>> {
    let _attribution = ::zeroclaw_log::attribution_span!(&ConfigLoadAttribution).entered();
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display().to_string()))?;
    let (migrated, notices) = match migrate_file_with_notices(&raw)? {
        Some(migrated) => migrated,
        None => return Ok(None),
    };
    // The `.backup` slot below is replaced by every migration, including a
    // later retirement-only one of a current file. Keep the pre-upgrade
    // original as well, once per version, where nothing replaces it.
    let from = detect_version(&toml::from_str(&raw).context("failed to parse config TOML")?)?;
    if from < CURRENT_SCHEMA_VERSION {
        keep_version_backup(path, from)?;
    }
    let parent = path.parent().with_context(|| {
        format!(
            "config path {} has no parent directory",
            path.display().to_string()
        )
    })?;
    let file_name = path.file_name().and_then(|s| s.to_str()).with_context(|| {
        format!(
            "config path {} has no file name",
            path.display().to_string()
        )
    })?;
    let backup_path = parent.join(format!("{file_name}.backup"));
    let temp_path = parent.join(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));

    // 1. Write migrated content to temp + fsync.
    {
        let mut temp = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| {
                format!(
                    "failed to create temporary migrated config at {}",
                    temp_path.display()
                )
            })?;
        std::io::Write::write_all(&mut temp, migrated.as_bytes()).with_context(|| {
            format!(
                "failed to write migrated config to {}",
                temp_path.display().to_string()
            )
        })?;
        temp.sync_all().with_context(|| {
            format!(
                "failed to fsync temporary migrated config at {}",
                temp_path.display()
            )
        })?;
    }

    // 2. Backup original BEFORE touching the destination. Copy gets a fresh inode.
    std::fs::copy(path, &backup_path).with_context(|| {
        format!(
            "failed to write backup {} before migration (temp file intact at {})",
            backup_path.display().to_string(),
            temp_path.display().to_string(),
        )
    })?;

    // 3. Atomic rename. On failure, restore from backup so the operator
    //    never observes a partial write.
    if let Err(rename_err) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        if backup_path.exists() {
            let _ = std::fs::copy(&backup_path, path);
        }
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "path": path.display().to_string(),
                    "backup_path": backup_path.display().to_string(),
                    "error": format!("{}", rename_err),
                })),
            "atomic rename failed during config migration"
        );
        anyhow::bail!(
            "failed to atomically replace {} with migrated config: {rename_err} \
             (backup retained at {})",
            path.display().to_string(),
            backup_path.display().to_string(),
        );
    }

    // 4. Fsync the parent directory so the rename is durable across crashes.
    sync_directory(parent).with_context(|| {
        format!(
            "failed to fsync parent directory after migration: {}",
            parent.display()
        )
    })?;

    Ok(Some(MigrateReport {
        backup_path,
        to_version: CURRENT_SCHEMA_VERSION,
        notices,
    }))
}

/// Where the copy of a config file at schema `version` is kept before this
/// binary first rewrites it: `<name>.v<version>.backup` beside it.
pub fn version_backup_path(config_path: &Path, version: u32) -> Result<std::path::PathBuf> {
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("config path {} has no file name", config_path.display()))?;
    Ok(config_path.with_file_name(format!("{file_name}.v{version}.backup")))
}

/// Copy `config_path` to [`version_backup_path`] unless that copy already
/// exists. It is written once and never replaced, so the original of each
/// version survives every later migration and save. The copy keeps the
/// file's permissions, since it holds the same secrets.
pub fn keep_version_backup(config_path: &Path, version: u32) -> Result<()> {
    let backup = version_backup_path(config_path, version)?;
    if backup.exists() {
        return Ok(());
    }
    std::fs::copy(config_path, &backup).with_context(|| {
        format!(
            "failed to keep a copy of {} as {} before rewriting it",
            config_path.display(),
            backup.display()
        )
    })?;
    Ok(())
}

/// Fsync the directory entry so a subsequent rename inside it is durable.
/// No-op on platforms where directory fsync isn't a meaningful primitive.
#[allow(clippy::unused_async)] // kept sync to mirror Config::save()'s helper
fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(path).with_context(|| {
            format!(
                "failed to open directory for fsync: {}",
                path.display().to_string()
            )
        })?;
        dir.sync_all().with_context(|| {
            format!("failed to fsync directory: {}", path.display().to_string())
        })?;
    }
    #[cfg(not(unix))]
    {
        // Best-effort: open + drop. Windows doesn't provide a portable
        // directory-fsync primitive in std; the rename itself is durable
        // on NTFS.
        let _ = std::fs::File::open(path);
    }
    Ok(())
}

/// Result of an on-disk migration. Returned by `migrate_file_in_place` when
/// migration ran (vs. `Ok(None)` when input was already current).
#[derive(Debug, Clone)]
pub struct MigrateReport {
    pub backup_path: std::path::PathBuf,
    pub to_version: u32,
    /// What the migration changed or assumed, for the operator.
    pub notices: Vec<MigrationNotice>,
}

pub fn ensure_disk_at_current_version(path: &Path) -> Result<()> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow::Error::from(e)).with_context(|| {
                format!("failed to read config at {}", path.display().to_string())
            });
        }
    };
    let value: toml::Value =
        toml::from_str(&raw).context("failed to parse config TOML for version check")?;
    let from = detect_version(&value)?;
    if from == CURRENT_SCHEMA_VERSION {
        return Ok(());
    }
    if from > CURRENT_SCHEMA_VERSION {
        anyhow::bail!(
            "config at {} is schema_version {from}, newer than this binary supports ({})",
            path.display().to_string(),
            CURRENT_SCHEMA_VERSION,
        );
    }
    anyhow::bail!(
        "config at {} is schema_version {from}; run `zeroclaw config migrate` to update before modifying",
        path.display().to_string(),
    );
}

pub(crate) fn fold_string_into_array(
    table: &mut toml::Table,
    from_key: &str,
    to_key: &str,
) -> bool {
    let value = match table.remove(from_key) {
        Some(toml::Value::String(s)) if !s.is_empty() => s,
        Some(other) => {
            // Non-string: re-insert under from_key untouched (caller may handle).
            table.insert(from_key.to_string(), other);
            return false;
        }
        None => return false,
    };
    let entry = table
        .entry(to_key.to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    if let Some(arr) = entry.as_array_mut() {
        let already_present = arr.iter().any(|v| v.as_str() == Some(value.as_str()));
        if !already_present {
            arr.push(toml::Value::String(value));
        }
        true
    } else {
        // Existing to_key wasn't an array (unusual). Reinsert from_key as-is.
        table.insert(from_key.to_string(), toml::Value::String(value));
        false
    }
}

/// One typed migration step: `V_n` TOML → `V_{n+1}` TOML.
type MigrationStep = fn(toml::Value) -> Result<toml::Value>;

const MIGRATION_STEPS: &[MigrationStep] = &[
    // V0 → V1: padding so slot 0 is never indexed. V0 does not exist.
    Ok,
    // V1 → V2
    |value| {
        let v1: V1Config = value
            .try_into()
            .context("failed to deserialize input as V1 schema")?;
        let v2 = v1.migrate();
        toml::Value::try_from(v2).context("failed to serialize V2 intermediate")
    },
    // V2 → V3
    |value| {
        let v2: V2Config = value
            .try_into()
            .context("failed to deserialize as V2 schema")?;
        v2.migrate().context("failed to migrate V2 → V3")
    },
    // V3 → V4: no shape change; the chain applies the V4 entries in
    // `RETIRED_KEYS` after this step.
    |value| stamp_schema_version(value, 4),
];

const _: () = assert!(
    MIGRATION_STEPS.len() as u32 == CURRENT_SCHEMA_VERSION,
    "MIGRATION_STEPS must have exactly one entry per schema version \
     (length = CURRENT_SCHEMA_VERSION, including the slot-0 padding)",
);

/// Run the typed migration chain from `from` up to `CURRENT_SCHEMA_VERSION`.
/// `from` must be `< CURRENT_SCHEMA_VERSION` (caller checks).
fn run_chain(
    value: toml::Value,
    from: u32,
    notices: &mut Vec<MigrationNotice>,
) -> Result<toml::Value> {
    run_chain_until(value, from, CURRENT_SCHEMA_VERSION, notices)
}

fn run_chain_until(
    value: toml::Value,
    from: u32,
    target: u32,
    notices: &mut Vec<MigrationNotice>,
) -> Result<toml::Value> {
    if target < from {
        anyhow::bail!("cannot migrate backwards from V{from} to V{target}");
    }
    if target > CURRENT_SCHEMA_VERSION {
        anyhow::bail!(
            "target V{target} exceeds CURRENT_SCHEMA_VERSION (V{CURRENT_SCHEMA_VERSION})"
        );
    }

    let mut cur = value;
    for (index, step) in MIGRATION_STEPS
        .iter()
        .enumerate()
        .take(target as usize)
        .skip(from as usize)
    {
        cur = step(cur)?;
        // Step `index` produces schema version `index + 1`. Apply every key
        // retired at or before that version: a file starting past an entry's
        // version (a V3 file holding a key retired at V2) still loses it,
        // while a key retired later survives until the step that retires it,
        // so a structural step can still read it first.
        for version in 2..=index as u32 + 1 {
            apply_retired_keys(&mut cur, version, RETIRED_KEYS, notices);
        }
    }
    Ok(cur)
}

/// Apply every retired key, through the current schema version, to an
/// on-disk document in place. This is the same [`RETIRED_KEYS`] policy the
/// migration chain applies to a parsed value, used where a file is edited
/// rather than rewritten (an incremental save). Only retired keys change:
/// comments, formatting and every other entry, including ciphertext, are left
/// byte for byte. All three spellings are handled (`[a.b]` headers, dotted
/// keys and inline tables). Returns what changed, for the caller to report
/// once the file is safely written.
pub fn apply_retired_keys_to_doc(root: &mut toml_edit::Table) -> Vec<MigrationNotice> {
    let mut notices = Vec::new();
    for version in 2..=CURRENT_SCHEMA_VERSION {
        apply_retired_keys_to_doc_table(root, version, RETIRED_KEYS, &mut notices);
    }
    notices
}

fn apply_retired_keys_to_doc_table(
    root: &mut toml_edit::Table,
    version: u32,
    table: &[RetiredKey],
    notices: &mut Vec<MigrationNotice>,
) {
    for key in table.iter().filter(|key| key.retired_in == version) {
        for concrete in expand_doc_path(root, key.path) {
            let segments: Vec<&str> = concrete.iter().map(String::as_str).collect();
            let Some(taken) = take_doc_path(root, &segments, key.path) else {
                continue;
            };
            let from = segments.join(".");
            let notice = match key.retirement {
                Retirement::Remove | Retirement::RemoveChannel => MigrationNotice::Removed {
                    path: from,
                    reason: key.reason,
                },
                Retirement::Rename { to } => {
                    let target = fill_wildcards(to, key.path, &segments);
                    let target: Vec<&str> = target.iter().map(String::as_str).collect();
                    let to_path = target.join(".");
                    if put_doc_path_if_vacant(root, &target, taken) {
                        MigrationNotice::Renamed {
                            from,
                            to: to_path,
                            reason: key.reason,
                        }
                    } else {
                        MigrationNotice::RenameConflict {
                            from,
                            to: to_path,
                            reason: key.reason,
                        }
                    }
                }
            };
            notices.push(notice);
        }
        if let Some(channel_type) = retired_channel_type(key) {
            prune_doc_channel_references(root, channel_type, key.reason, notices);
        }
    }
}

/// The channel type a [`Retirement::RemoveChannel`] entry retires, or `None`
/// for every other retirement.
fn retired_channel_type(key: &RetiredKey) -> Option<&'static str> {
    match (key.retirement, key.path) {
        (Retirement::RemoveChannel, ["channels", channel_type]) => Some(*channel_type),
        _ => None,
    }
}

/// The channel type a channel reference names: `"telegram"` names the type
/// itself, `"telegram.work"` one alias of it. The reference is trimmed first,
/// as `Config::validate` and the `alias_refs` cascade trim it.
fn channel_reference_type(reference: &str) -> &str {
    let reference = reference.trim();
    reference.split('.').next().unwrap_or(reference)
}

/// Lists a channel reference can be dropped from: every `[agents.<alias>]
/// channels` and `[escalation] alert_channels`. With
/// [`CHANNEL_REFERENCE_BINDINGS`] these mirror the reference sites of the
/// canonical cascade in `alias_refs` (`collect_channel_refs`), which the test
/// `channel_reference_sites_match_the_alias_cascade` keeps in step.
const CHANNEL_REFERENCE_LISTS: &[&[&str]] = &[
    &["agents", ANY_KEY, "channels"],
    &["escalation", "alert_channels"],
];

/// Tables bound to a channel by one reference: `[peer_groups.<name>]
/// channel`. The cascade treats this as a hard reference and refuses to
/// delete the group, so a retirement leaves it in place too.
const CHANNEL_REFERENCE_BINDINGS: &[&[&str]] = &[&["peer_groups", ANY_KEY, "channel"]];

/// The list whose entries bind agents to channels. Emptying the last of them
/// changes routing, not just references: with no binding anywhere, the
/// runtime hands every configured channel to the fallback agent.
const AGENT_CHANNEL_BINDINGS: &[&str] = &["agents", ANY_KEY, "channels"];

const KEPT_PEER_GROUP: &str = "a peer group is not deleted when its channel is retired, because \
     its members and settings are the operator's; rebind it to a live channel or remove it";

const KEPT_LAST_BINDING: &str = "removing it would leave no agent bound to any channel, which \
     routes every configured channel to the fallback agent; bind an agent to a live channel, \
     then remove this entry";

/// What a retirement does with the references to a retired channel type.
struct ChannelReferencePlan {
    /// Whether the agent channel bindings may be pruned: false when doing so
    /// would leave no binding anywhere (see [`AGENT_CHANNEL_BINDINGS`]).
    prune_agent_bindings: bool,
}

impl ChannelReferencePlan {
    /// Decide from the counts of agent channel entries before and after the
    /// retired type's entries are dropped.
    fn new(bindings_before: usize, bindings_after: usize) -> Self {
        Self {
            prune_agent_bindings: bindings_before == 0 || bindings_after > 0,
        }
    }
}

/// Handle every reference to the retired `channel_type` in a parsed config:
/// drop it from the lists in [`CHANNEL_REFERENCE_LISTS`], except that agent
/// bindings are kept when pruning them would leave none; keep each peer group
/// bound to it. Every removal and every kept reference gets a notice.
fn prune_channel_references(
    root: &mut toml::Table,
    channel_type: &str,
    reason: &'static str,
    notices: &mut Vec<MigrationNotice>,
) {
    let retired = |reference: &str| channel_reference_type(reference) == channel_type;
    let entries = |root: &toml::Table, keep_retired: bool| -> usize {
        expand_path(root, AGENT_CHANNEL_BINDINGS)
            .iter()
            .filter_map(|path| value_at(root, path).and_then(toml::Value::as_array))
            .flatten()
            .filter(|entry| keep_retired || !entry.as_str().is_some_and(retired))
            .count()
    };
    let plan = ChannelReferencePlan::new(entries(root, true), entries(root, false));

    for pattern in CHANNEL_REFERENCE_LISTS {
        let prune = *pattern != AGENT_CHANNEL_BINDINGS || plan.prune_agent_bindings;
        for path in expand_path(root, pattern) {
            let joined = path.join(".");
            let Some(toml::Value::Array(list)) = value_at_mut(root, &path) else {
                continue;
            };
            list.retain(|entry| match entry.as_str() {
                Some(reference) if retired(reference) => {
                    notices.push(channel_reference_notice(prune, &joined, reference, reason));
                    !prune
                }
                _ => true,
            });
        }
    }
    for pattern in CHANNEL_REFERENCE_BINDINGS {
        for path in expand_path(root, pattern) {
            if let Some(reference) = value_at(root, &path).and_then(toml::Value::as_str)
                && retired(reference)
            {
                notices.push(MigrationNotice::ReferenceKept {
                    path: path.join("."),
                    reference: reference.to_string(),
                    reason: KEPT_PEER_GROUP,
                });
            }
        }
    }
}

/// [`prune_channel_references`] over a document, in place, for every table
/// spelling (`[a.b]` headers, dotted keys, inline tables).
fn prune_doc_channel_references(
    root: &mut toml_edit::Table,
    channel_type: &str,
    reason: &'static str,
    notices: &mut Vec<MigrationNotice>,
) {
    let retired = |reference: &str| channel_reference_type(reference) == channel_type;
    let entries = |root: &toml_edit::Table, keep_retired: bool| -> usize {
        expand_doc_path(root, AGENT_CHANNEL_BINDINGS)
            .iter()
            .filter_map(|path| doc_item_at(root, path).and_then(toml_edit::Item::as_array))
            .flat_map(toml_edit::Array::iter)
            .filter(|entry| keep_retired || !entry.as_str().is_some_and(retired))
            .count()
    };
    let plan = ChannelReferencePlan::new(entries(root, true), entries(root, false));

    for pattern in CHANNEL_REFERENCE_LISTS {
        let prune = *pattern != AGENT_CHANNEL_BINDINGS || plan.prune_agent_bindings;
        for path in expand_doc_path(root, pattern) {
            let joined = path.join(".");
            let Some(list) = doc_item_at_mut(root, &path).and_then(toml_edit::Item::as_array_mut)
            else {
                continue;
            };
            let before = list.len();
            list.retain(|entry| match entry.as_str() {
                Some(reference) if retired(reference) => {
                    notices.push(channel_reference_notice(prune, &joined, reference, reason));
                    !prune
                }
                _ => true,
            });
            // The removed first entry's successor keeps the space that
            // separated it from the comma; drop it so `[ "a"]` reads `["a"]`.
            if list.len() != before
                && let Some(first) = list.get_mut(0)
            {
                first.decor_mut().set_prefix("");
            }
        }
    }
    for pattern in CHANNEL_REFERENCE_BINDINGS {
        for path in expand_doc_path(root, pattern) {
            if let Some(reference) = doc_item_at(root, &path).and_then(toml_edit::Item::as_str)
                && retired(reference)
            {
                notices.push(MigrationNotice::ReferenceKept {
                    path: path.join("."),
                    reference: reference.to_string(),
                    reason: KEPT_PEER_GROUP,
                });
            }
        }
    }
}

/// The notice for one retired channel reference found in a list: removed, or
/// kept because it is one of the last agent bindings.
fn channel_reference_notice(
    removed: bool,
    path: &str,
    reference: &str,
    reason: &'static str,
) -> MigrationNotice {
    if removed {
        MigrationNotice::ReferenceRemoved {
            path: path.to_string(),
            reference: reference.to_string(),
            reason,
        }
    } else {
        MigrationNotice::ReferenceKept {
            path: path.to_string(),
            reference: reference.to_string(),
            reason: KEPT_LAST_BINDING,
        }
    }
}

/// The value at a concrete path in a parsed config.
fn value_at<'a>(root: &'a toml::Table, path: &[String]) -> Option<&'a toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut table = root;
    for segment in parents {
        table = table.get(segment)?.as_table()?;
    }
    table.get(last)
}

/// [`value_at`], mutably.
fn value_at_mut<'a>(root: &'a mut toml::Table, path: &[String]) -> Option<&'a mut toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut table = root;
    for segment in parents {
        table = table.get_mut(segment)?.as_table_mut()?;
    }
    table.get_mut(last)
}

/// [`value_at`] over a document, through every table spelling.
fn doc_item_at<'a>(root: &'a toml_edit::Table, path: &[String]) -> Option<&'a toml_edit::Item> {
    let (last, parents) = path.split_last()?;
    let mut table: &dyn toml_edit::TableLike = root;
    for segment in parents {
        table = table.get(segment)?.as_table_like()?;
    }
    table.get(last)
}

/// [`doc_item_at`], mutably.
fn doc_item_at_mut<'a>(
    root: &'a mut toml_edit::Table,
    path: &[String],
) -> Option<&'a mut toml_edit::Item> {
    let (last, parents) = path.split_last()?;
    let mut table: &mut dyn toml_edit::TableLike = root;
    for segment in parents {
        table = table.get_mut(segment)?.as_table_like_mut()?;
    }
    table.get_mut(last)
}

/// [`expand_path`] over a document: every concrete path `pattern` matches,
/// descending through header, dotted and inline tables alike.
fn expand_doc_path(root: &toml_edit::Table, pattern: &[&str]) -> Vec<Vec<String>> {
    fn walk(
        table: &dyn toml_edit::TableLike,
        pattern: &[&str],
        prefix: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
    ) {
        let Some((segment, rest)) = pattern.split_first() else {
            return;
        };
        let keys: Vec<String> = if *segment == ANY_KEY {
            table.iter().map(|(key, _)| key.to_string()).collect()
        } else if table.contains_key(segment) {
            vec![(*segment).to_string()]
        } else {
            Vec::new()
        };
        for key in keys {
            prefix.push(key.clone());
            if rest.is_empty() {
                out.push(prefix.clone());
            } else if let Some(next) = table.get(&key).and_then(toml_edit::Item::as_table_like) {
                walk(next, rest, prefix, out);
            }
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(root, pattern, &mut Vec::new(), &mut out);
    out
}

/// [`take_path`] over a document, with the same [`prunable`] rule for
/// containers the removal leaves empty.
fn take_doc_path(
    table: &mut dyn toml_edit::TableLike,
    path: &[&str],
    pattern: &[&str],
) -> Option<toml_edit::Item> {
    let (first, rest) = path.split_first()?;
    if rest.is_empty() {
        return table.remove(first);
    }
    let child = table.get_mut(first)?.as_table_like_mut()?;
    let taken = take_doc_path(child, rest, pattern.get(1..).unwrap_or_default())?;
    if child.is_empty() {
        if prunable(pattern) {
            table.remove(first);
        } else if let Some(alias) = table.get_mut(first) {
            keep_empty_table_rendered(alias);
        }
    }
    Some(taken)
}

/// Make an empty table serialize. `toml_edit` writes nothing for an empty
/// table that is implicit (it only existed as a parent of other headers) or
/// dotted (it only existed as a prefix of dotted keys, such as
/// `runtime_profiles.slow.x = 1`); an empty dotted inline table vanishes the
/// same way. Dropping such a table would silently delete an operator's alias
/// and leave any reference to it dangling, so a header table gets its own
/// `[a.b]` header and an inline table is written as `b = {}`.
fn keep_empty_table_rendered(item: &mut toml_edit::Item) {
    match item {
        toml_edit::Item::Table(table) => {
            table.set_dotted(false);
            table.set_implicit(false);
        }
        toml_edit::Item::Value(toml_edit::Value::InlineTable(table)) => table.set_dotted(false),
        _ => {}
    }
}

/// [`put_path_if_vacant`] over a document. Missing parents are created as
/// implicit tables, so no empty header is written for them.
fn put_doc_path_if_vacant(
    root: &mut toml_edit::Table,
    path: &[&str],
    item: toml_edit::Item,
) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    // Check the whole path first, so a refusal creates no parent tables.
    let mut probe: Option<&dyn toml_edit::TableLike> = Some(&*root);
    for segment in parents {
        probe = match probe.and_then(|table| table.get(segment)) {
            None => None,
            Some(item) => match item.as_table_like() {
                Some(next) => Some(next),
                None => return false,
            },
        };
    }
    if probe.is_some_and(|table| table.contains_key(last)) {
        return false;
    }
    let mut table: &mut dyn toml_edit::TableLike = root;
    for segment in parents {
        if !table.contains_key(segment) {
            let mut implicit = toml_edit::Table::new();
            implicit.set_implicit(true);
            table.insert(segment, toml_edit::Item::Table(implicit));
        }
        let Some(next) = table
            .get_mut(segment)
            .and_then(toml_edit::Item::as_table_like_mut)
        else {
            return false;
        };
        table = next;
    }
    table.insert(last, item);
    true
}

/// Give every table on the path to a retired key an explicit header if the
/// retirement left it empty. An implicit table renders as nothing once empty,
/// so without this an alias whose only content was retired (for example a
/// runtime profile that only set `context_compression.summary_model`) would
/// vanish from the written file while it still exists in the migrated value,
/// leaving any reference to it dangling.
fn keep_emptied_tables_visible(root: &mut toml_edit::Table, notices: &[MigrationNotice]) {
    for notice in notices {
        let from = match notice {
            MigrationNotice::Removed { path, .. } => path,
            MigrationNotice::Renamed { from, .. }
            | MigrationNotice::RenameConflict { from, .. } => from,
            MigrationNotice::AssumedV1
            | MigrationNotice::InferredV3
            | MigrationNotice::ReferenceRemoved { .. }
            | MigrationNotice::ReferenceKept { .. }
            | MigrationNotice::IgnoredEnvOverride { .. } => continue,
        };
        let segments: Vec<&str> = from.split('.').collect();
        let parents = &segments[..segments.len().saturating_sub(1)];
        let mut table: &mut dyn toml_edit::TableLike = &mut *root;
        for segment in parents {
            let Some(item) = table.get_mut(segment) else {
                break;
            };
            if item
                .as_table_like()
                .is_some_and(toml_edit::TableLike::is_empty)
            {
                keep_empty_table_rendered(item);
            }
            let Some(next) = item.as_table_like_mut() else {
                break;
            };
            table = next;
        }
    }
}

fn stamp_schema_version(mut value: toml::Value, version: u32) -> Result<toml::Value> {
    value
        .as_table_mut()
        .context("config root must be a TOML table")?
        .insert(
            "schema_version".to_string(),
            toml::Value::Integer(i64::from(version)),
        );
    Ok(value)
}

/// Apply every entry of `table` retired in `version`, recording a notice for
/// each key that was actually present. Keys that are absent change nothing.
/// An [`ANY_KEY`] segment expands to every key at its level, and each concrete
/// match gets its own notice naming its real path.
fn apply_retired_keys(
    value: &mut toml::Value,
    version: u32,
    table: &[RetiredKey],
    notices: &mut Vec<MigrationNotice>,
) {
    let Some(root) = value.as_table_mut() else {
        return;
    };
    for key in table.iter().filter(|key| key.retired_in == version) {
        for concrete in expand_path(root, key.path) {
            let segments: Vec<&str> = concrete.iter().map(String::as_str).collect();
            let Some(taken) = take_path(root, &segments, key.path) else {
                continue;
            };
            let from = segments.join(".");
            let notice = match key.retirement {
                Retirement::Remove | Retirement::RemoveChannel => MigrationNotice::Removed {
                    path: from,
                    reason: key.reason,
                },
                Retirement::Rename { to } => {
                    let target = fill_wildcards(to, key.path, &segments);
                    let target: Vec<&str> = target.iter().map(String::as_str).collect();
                    let to_path = target.join(".");
                    if put_path_if_vacant(root, &target, taken) {
                        MigrationNotice::Renamed {
                            from,
                            to: to_path,
                            reason: key.reason,
                        }
                    } else {
                        MigrationNotice::RenameConflict {
                            from,
                            to: to_path,
                            reason: key.reason,
                        }
                    }
                }
            };
            notices.push(notice);
        }
        if let Some(channel_type) = retired_channel_type(key) {
            prune_channel_references(root, channel_type, key.reason, notices);
        }
    }
}

/// Every concrete path in `root` that `pattern` matches, expanding each
/// [`ANY_KEY`] to the keys present at that level. A literal segment matches
/// only itself; intermediate segments must be tables.
fn expand_path(root: &toml::Table, pattern: &[&str]) -> Vec<Vec<String>> {
    fn walk(
        table: &toml::Table,
        pattern: &[&str],
        prefix: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
    ) {
        let Some((segment, rest)) = pattern.split_first() else {
            return;
        };
        let keys: Vec<&String> = if *segment == ANY_KEY {
            table.keys().collect()
        } else {
            table
                .get_key_value(*segment)
                .map(|(k, _)| k)
                .into_iter()
                .collect()
        };
        for key in keys {
            prefix.push(key.clone());
            if rest.is_empty() {
                out.push(prefix.clone());
            } else if let Some(next) = table.get(key.as_str()).and_then(toml::Value::as_table) {
                walk(next, rest, prefix, out);
            }
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(root, pattern, &mut Vec::new(), &mut out);
    out
}

/// Fill each [`ANY_KEY`] in `target` with the key the corresponding
/// [`ANY_KEY`] in `pattern` matched in `matched`, in order.
fn fill_wildcards(target: &[&str], pattern: &[&str], matched: &[&str]) -> Vec<String> {
    let mut captured = pattern
        .iter()
        .zip(matched)
        .filter(|(segment, _)| **segment == ANY_KEY)
        .map(|(_, key)| (*key).to_string());
    target
        .iter()
        .map(|segment| {
            if *segment == ANY_KEY {
                captured.next().unwrap_or_else(|| ANY_KEY.to_string())
            } else {
                (*segment).to_string()
            }
        })
        .collect()
}

/// Remove and return the value at `path`, if every segment exists. A
/// container the removal leaves empty is dropped too when [`prunable`] allows
/// it for that level of `pattern`.
fn take_path(root: &mut toml::Table, path: &[&str], pattern: &[&str]) -> Option<toml::Value> {
    let (first, rest) = path.split_first()?;
    if rest.is_empty() {
        return root.remove(*first);
    }
    let child = root.get_mut(*first)?.as_table_mut()?;
    let taken = take_path(child, rest, pattern.get(1..).unwrap_or_default())?;
    if child.is_empty() && prunable(pattern) {
        root.remove(*first);
    }
    Some(taken)
}

/// Whether a container matched by the first segment of `pattern` may be
/// dropped once a retirement leaves it empty. A literal segment names
/// structure that exists only to hold keys (`security`, `context_compression`),
/// so an empty one carries nothing. An [`ANY_KEY`] segment matches an
/// operator-named alias (`[agents.coder]`), whose existence is meaningful even
/// when empty, so it is always kept.
fn prunable(pattern: &[&str]) -> bool {
    pattern.first().is_some_and(|segment| *segment != ANY_KEY)
}

/// Insert `value` at `path`, creating missing parent tables. Returns `false`,
/// leaving the config unchanged, when `path` is already set or a parent
/// segment is not a table.
fn put_path_if_vacant(root: &mut toml::Table, path: &[&str], value: toml::Value) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    // Check the whole path first, so a refusal creates no parent tables.
    let mut probe = Some(&*root);
    for segment in parents {
        probe = match probe.and_then(|table| table.get(*segment)) {
            None => None,
            Some(toml::Value::Table(next)) => Some(next),
            Some(_) => return false,
        };
    }
    if probe.is_some_and(|table| table.contains_key(*last)) {
        return false;
    }
    let mut table = root;
    for segment in parents {
        let entry = table
            .entry((*segment).to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(next) = entry.as_table_mut() else {
            return false;
        };
        table = next;
    }
    table.insert((*last).to_string(), value);
    true
}

pub(crate) fn sync_table(doc: &mut toml_edit::Table, new: &toml::Table) {
    // Drop keys not present in new
    let to_remove: Vec<String> = doc
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !new.contains_key(k))
        .collect();
    for k in to_remove {
        doc.remove(&k);
    }

    for (key, new_value) in new.iter() {
        if let (Some(doc_item), toml::Value::Table(new_sub)) =
            (doc.get_mut(key.as_str()), new_value)
            && let Some(doc_sub) = doc_item.as_table_mut()
        {
            // Both tables — recurse to preserve nested comments.
            sync_table(doc_sub, new_sub);
            continue;
        }
        // Otherwise, replace the value while preserving the key's leading decor.
        let new_item = toml_value_to_edit_item(new_value);
        match doc.get_mut(key.as_str()) {
            Some(existing) => {
                // Preserve the key's leading decor (comments) by mutating in place.
                *existing = new_item;
            }
            None => {
                doc.insert(key.as_str(), new_item);
            }
        }
    }
}

/// Convert a `toml::Value` into a `toml_edit::Item` for insertion into
/// a `DocumentMut`. Tables become inline tables when small, real tables
/// otherwise — matches `toml_edit`'s default round-trip behavior.
pub(crate) fn toml_value_to_edit_item(value: &toml::Value) -> toml_edit::Item {
    // Easiest path: serialize to string, parse as toml_edit. Lossy on numeric
    // formatting nuance but correct for migration round-trip where we're
    // emitting freshly-serialized values.
    let serialized = match value {
        toml::Value::Table(t) => {
            let mut wrapper = toml::Table::new();
            wrapper.insert("__v".into(), toml::Value::Table(t.clone()));
            toml::to_string(&wrapper).unwrap_or_default()
        }
        other => {
            let mut wrapper = toml::Table::new();
            wrapper.insert("__v".into(), other.clone());
            toml::to_string(&wrapper).unwrap_or_default()
        }
    };
    let doc: toml_edit::DocumentMut = serialized.parse().unwrap_or_default();
    doc.get("__v").cloned().unwrap_or(toml_edit::Item::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_version_missing_is_v1() {
        let v: toml::Value = toml::from_str("foo = 1").unwrap();
        assert_eq!(detect_version(&v).unwrap(), 1);
    }

    #[test]
    fn detect_version_explicit() {
        let v: toml::Value = toml::from_str("schema_version = 2\n").unwrap();
        assert_eq!(detect_version(&v).unwrap(), 2);
    }

    #[test]
    fn detect_version_negative_errors() {
        let v: toml::Value = toml::from_str("schema_version = -1\n").unwrap();
        assert!(detect_version(&v).is_err());
    }

    #[test]
    fn detect_version_string_errors() {
        let v: toml::Value = toml::from_str("schema_version = \"two\"\n").unwrap();
        assert!(detect_version(&v).is_err());
    }

    // ── resilient daemon load: starts no matter what, so config can be repaired ──

    #[test]
    fn broken_channel_alias_is_dropped_not_fatal() {
        // Email alias missing required `imap_host` must not abort the load.
        let raw = r#"
schema_version = 4

[channels.email.fakeemail]
enabled = true
smtp_host = "smtp.example.com"
username = "u"
password = "p"
from_address = "a@example.com"
"#;
        let cfg = migrate_to_current_resilient(raw);
        assert!(
            !cfg.channels.email.contains_key("fakeemail"),
            "invalid alias must be pruned"
        );
    }

    #[test]
    fn partial_telegram_alias_survives_salvage() {
        // A Telegram alias with no `bot_token` (e.g. just created via
        // create_map_key, then round-tripped through save_dirty's
        // prune_empty_leaves, which strips the empty string) must survive
        // salvage instead of being dropped: `bot_token` now has
        // `#[serde(default)]`, so a missing token deserializes the same as
        // an explicit `bot_token = ""`. Runtime safety is enforced
        // separately by `validate_bot_token` when `enabled = true`.
        let raw = r#"
schema_version = 4

[channels.telegram.default]
enabled = true
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.config.channels.telegram.contains_key("default"),
            "a partial (tokenless) alias must survive salvage, got {:?}",
            load.config.channels.telegram.keys().collect::<Vec<_>>()
        );
        assert!(
            load.dropped.is_empty(),
            "a partial (tokenless) alias must not be reported as dropped, got {:?}",
            load.dropped
        );
    }

    #[test]
    fn corrupt_telegram_alias_is_still_dropped_and_recorded() {
        // Guard that salvage still prunes genuine garbage: a `bot_token`
        // with the wrong type (int instead of string) is a real type error,
        // not merely a missing field, and must still be dropped with the
        // exact path recorded so `doctor` can name it (see zeroclaw-runtime's
        // check_degraded_sections).
        let raw = r#"
schema_version = 4

[channels.telegram.bad]
enabled = true
bot_token = 42
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            !load.config.channels.telegram.contains_key("bad"),
            "type-corrupt alias must be pruned, got {:?}",
            load.config.channels.telegram.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            load.dropped,
            vec!["channels.telegram.bad"],
            "dropped list must pin the exact malformed section path, got {:?}",
            load.dropped
        );
    }

    #[test]
    fn partial_discord_alias_survives_salvage() {
        // Discord twin of partial_telegram_alias_survives_salvage: a Discord
        // alias with no `bot_token` must survive salvage now that
        // `DiscordConfig.bot_token` also has `#[serde(default)]`.
        let raw = r#"
schema_version = 4

[channels.discord.default]
enabled = true
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.config.channels.discord.contains_key("default"),
            "a partial (tokenless) alias must survive salvage, got {:?}",
            load.config.channels.discord.keys().collect::<Vec<_>>()
        );
        assert!(
            load.dropped.is_empty(),
            "a partial (tokenless) alias must not be reported as dropped, got {:?}",
            load.dropped
        );
    }

    #[test]
    fn complete_telegram_alias_survives() {
        // Companion to partial_telegram_alias_survives_salvage and
        // corrupt_telegram_alias_is_still_dropped_and_recorded: a complete
        // [channels.telegram.default] (bot_token present) must survive
        // intact and must not appear in `dropped`.
        let raw = r#"
schema_version = 4

[channels.telegram.default]
enabled = true
bot_token = "t"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.config.channels.telegram.contains_key("default"),
            "a complete alias must survive salvage"
        );
        assert!(
            load.dropped.is_empty(),
            "a complete alias must not be reported as dropped, got {:?}",
            load.dropped
        );
    }

    #[test]
    fn valid_provider_aliases_survive_broken_sibling() {
        // Repro for the zerocode "all providers vanish after restart" report:
        // one malformed provider alias must not take the whole [providers]
        // section (and every other provider) down with it.
        let raw = r#"
schema_version = 4

[providers.models.ollama.ai]
model = "qwen3:30b"

[providers.models.custom.rag_bot]
uri = "http://localhost:8000/v1"
model = "m"

[providers.models.custom.broken]
uri = "http://localhost:9000/v1"
model = "m"
temperature = "hot"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert_eq!(load.dropped, vec!["providers.models.custom.broken"]);
        assert!(
            load.config.providers.models.find("ollama", "ai").is_some(),
            "valid alias in another family must survive"
        );
        assert!(
            load.config
                .providers
                .models
                .find("custom", "rag_bot")
                .is_some(),
            "valid sibling alias must survive"
        );
        assert!(
            load.config
                .providers
                .models
                .find("custom", "broken")
                .is_none(),
            "only the malformed alias is pruned"
        );
    }

    #[test]
    fn v2_bare_vision_provider_reference_migrates_to_dotted_alias() {
        // Repro: a bare `[multimodal] vision_model_provider` cannot
        // select the migrated V3 alias, so the keyed provider's credentials
        // never reach the vision route. Migration must rewrite the reference
        // to the family's unambiguous migrated alias.
        let raw = r#"
schema_version = 2

[providers.models.openrouter]
api_key = "sk-openrouter-test"
model = "a-vision-capable-openrouter-model"

[multimodal]
vision_model_provider = "openrouter"
vision_model = "a-vision-capable-openrouter-model"

[media_pipeline]
enabled = true
describe_images = true
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openrouter.default"),
            "bare reference must become a dotted alias ref"
        );
        let alias = cfg
            .providers
            .models
            .find("openrouter", "default")
            .expect("migrated alias must exist");
        assert_eq!(
            alias.api_key.as_deref(),
            Some("sk-openrouter-test"),
            "dotted reference must select the migrated alias credential"
        );
    }

    #[test]
    fn v2_dotted_vision_provider_reference_preserved() {
        // An explicit dotted reference already selects the migrated alias;
        // migration must leave it unchanged.
        let raw = r#"
schema_version = 2

[providers.models.openrouter]
api_key = "sk-openrouter-test"
model = "m"

[multimodal]
vision_model_provider = "openrouter.default"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openrouter.default"),
            "explicit dotted reference must be preserved unchanged"
        );
    }

    #[test]
    fn v2_bare_vision_provider_reference_without_alias_left_alone() {
        // A bare family with no migrated alias stays bare so the runtime
        // keeps failing closed on an unknown provider.
        let raw = r#"
schema_version = 2

[providers.models.openrouter]
api_key = "sk-openrouter-test"
model = "m"

[multimodal]
vision_model_provider = "nonexistent"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("nonexistent"),
            "bare reference to an unknown family must not be rewritten"
        );
    }

    #[test]
    fn v2_legacy_grok_vision_reference_migrates_to_xai_default() {
        // `grok` canonicalizes to the xai family; the bare reference must be
        // resolved through the same mapping and rewrite to xai.default.
        let raw = r#"
schema_version = 2

[providers.models.grok]
api_key = "sk-grok-test"
model = "m"

[multimodal]
vision_model_provider = "grok"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "legacy grok reference must rewrite to the canonical xai.default alias"
        );
        assert!(
            cfg.providers.models.find("xai", "default").is_some(),
            "migrated grok entry must live at xai.default"
        );
    }

    #[test]
    fn v2_legacy_source_with_folded_globals_still_migrates() {
        // A sole legacy source (`[providers.models.grok]`) plus global
        // `[providers]` values with no explicit `default_provider`: the fold
        // reuses the already-materialized `xai` alias table (the only
        // family present) rather than introducing a second distinct source.
        // Registering the canonical family name as a second provenance
        // producer here would falsely make the slot look ambiguous and
        // leave the bare reference unrewritten even though `grok` is the
        // sole real source.
        let raw = r#"
schema_version = 2

[providers]
api_key = "sk-global-test"
default_model = "vision-model"

[providers.models.grok]

[multimodal]
vision_model_provider = "grok"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "the sole legacy source must still resolve the bare reference \
             even after global values are folded into the same slot"
        );
        let alias = cfg
            .providers
            .models
            .find("xai", "default")
            .expect("global values must fold into the migrated xai.default alias");
        assert_eq!(alias.api_key.as_deref(), Some("sk-global-test"));
        assert_eq!(alias.model.as_deref(), Some("vision-model"));
    }

    #[test]
    fn v2_explicit_default_provider_overlay_keeps_single_producer() {
        // Explicit `default_provider = "xai"` names the same slot a legacy
        // `[providers.models.grok]` already materialized (grok -> xai.default).
        // The globals fold overlays that existing slot; counting `xai` as a
        // second producer would make the slot look ambiguous and leave the
        // bare `grok` reference unrewritten, losing the typed credentials on
        // the runtime's bare-provider path.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "xai"
api_key = "global-test-key"
default_model = "vision-model"

[providers.models.grok]
api_key = "grok-test-key"
model = "grok-model"

[multimodal]
vision_model_provider = "grok"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "the explicit default_provider overlay must not double-count the \
             existing producer or strand the bare reference"
        );
        let alias = cfg
            .providers
            .models
            .find("xai", "default")
            .expect("the migrated xai.default alias must exist");
        assert_eq!(alias.api_key.as_deref(), Some("grok-test-key"));
        assert_eq!(alias.model.as_deref(), Some("grok-model"));
    }

    #[test]
    fn v2_explicit_default_provider_variant_overlay_stays_fail_closed() {
        // `default_provider = "qwen-intl"` selects the international variant,
        // but the raw `qwen` entry already materialized `qwen.default` with the
        // cn endpoint. The fold is fill-only, so `endpoint = intl` cannot
        // replace the existing cn endpoint. The selector is therefore a
        // DIFFERENT source than the slot's recorded producer: it must register
        // as a distinct producer and leave the bare `qwen` reference fail-closed
        // rather than rewrite it to `qwen.default` and consume the global
        // credential against the cn endpoint.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "qwen-intl"
api_key = "global-test-key"
default_model = "vision-model"

[providers.models.qwen]
model = "canonical-model"

[multimodal]
vision_model_provider = "qwen"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen"),
            "a default_provider naming a different variant than the slot's \
             producer must leave the bare reference fail-closed"
        );
    }

    #[test]
    fn v2_explicit_default_provider_distinct_colon_url_stays_fail_closed() {
        // Two distinct colon-URL sources: the raw entry materializes
        // `custom.default` with uri A, while `default_provider =
        // "custom:https://B"` selects a different URL. The URL is part of the
        // source identity, so the selector is NOT an equivalent overlay of the
        // existing slot: it must stay a distinct producer and leave the bare
        // `custom` reference fail-closed rather than consume the credential
        // against URL A.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "custom:https://b.example.invalid/v1"
api_key = "global-test-key"
default_model = "vision-model"

[providers.models."custom:https://a.example.invalid/v1"]
model = "vision-model"

[multimodal]
vision_model_provider = "custom"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom"),
            "a default_provider naming a different colon-URL source than the \
             slot's producer must leave the bare reference fail-closed"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("the migrated custom.default alias must exist");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://a.example.invalid/v1"),
            "the slot's existing uri must survive the non-equivalent overlay"
        );
    }

    #[test]
    fn v2_explicit_default_provider_colon_url_over_bare_custom_stays_fail_closed() {
        // The existing source is a BARE `custom` entry (its uri lives in the
        // config), while `default_provider = "custom:https://B"` selects a
        // different URL. The URL is part of the selector's source identity, so
        // it is not an equivalent overlay of the bare producer. Registering the
        // selector under only the stripped `custom` prefix would dedupe against
        // the existing bare `custom` producer and leave a single-producer slot,
        // letting the bare vision reference rewrite to `custom.default` and
        // consume the global credential against URI A despite the selector
        // naming URL B.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "custom:https://b.example.invalid/v1"
api_key = "global-key"
default_model = "vision-model"

[providers.models.custom]
uri = "https://a.example.invalid/v1"
model = "vision-model"

[multimodal]
vision_model_provider = "custom"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom"),
            "a colon-URL default_provider over a bare custom producer must leave \
             the bare reference fail-closed rather than rewrite against URI A"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("the migrated custom.default alias must exist");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://a.example.invalid/v1"),
            "the bare producer's uri must survive the non-equivalent overlay"
        );
    }

    #[test]
    fn v2_matching_colon_url_default_provider_supplies_bare_custom_uri() {
        // The exact ordering case from review: `alias_provider_models` creates
        // `custom.default` from the bare `[providers.models.custom]` entry
        // WITHOUT a uri, then `default_provider = "custom:https://B"` selects
        // that same slot and the globals fold supplies the missing `uri` (and
        // the global credential). Equivalence must be judged against the
        // COMPLETED alias state: the pre-fold alias lacks the URI, but once the
        // fold fills it the final `custom.default` exactly matches the selector,
        // so the selector is an overlay of the sole producer, not a second one.
        // A pre-fold equivalence check would register `custom:https://B` as a
        // second producer, make the slot ambiguous, and leave the matching
        // colon-URL vision reference on the configless path with the migrated
        // key unreachable.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "custom:https://b.example.invalid/v1"
api_key = "global-key"
default_model = "vision-model"

[providers.models.custom]
model = "vision-model"

[multimodal]
vision_model_provider = "custom:https://b.example.invalid/v1"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom.default"),
            "the matching colon-URL reference must rewrite to the alias whose URI \
             the fold supplied"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("the migrated custom.default alias must exist");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://b.example.invalid/v1"),
            "the fold must supply the selector's URI to the bare custom alias"
        );
        assert_eq!(alias.api_key.as_deref(), Some("global-key"));
        assert_eq!(alias.model.as_deref(), Some("vision-model"));
    }

    #[test]
    fn v2_matching_colon_url_default_provider_creates_alias_from_scratch() {
        // The maintainer's exact repro: NO `[providers.models]` entry at all.
        // `alias_provider_models` materializes nothing, so the fold itself
        // creates `custom.default` from `default_provider = "custom:https://B"`
        // plus the global key and model. The completed alias matches the
        // selector's own identity, but that must NOT clear the producer: the
        // selector created the slot, so it stays the sole producer and the
        // matching colon-URL vision reference must still rewrite to
        // `custom.default` with the URI, key, and model preserved.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "custom:https://b.example.invalid/v1"
api_key = "global-key"
default_model = "vision-model"

[multimodal]
vision_model_provider = "custom:https://b.example.invalid/v1"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom.default"),
            "a default_provider that creates the alias must still be its producer \
             so the matching colon-URL reference rewrites"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("the selector-created custom.default alias must exist");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://b.example.invalid/v1"),
            "the selector's URI must be preserved on the created alias"
        );
        assert_eq!(alias.api_key.as_deref(), Some("global-key"));
        assert_eq!(alias.model.as_deref(), Some("vision-model"));
    }

    #[test]
    fn v2_globals_create_missing_default_alias_beside_non_default_alias() {
        // No `default_provider`, only a non-default alias (`openai.codex` from
        // `openai-codex`), and global `[providers]` values. The globals fold
        // creates the missing `openai.default` alias; that fold must be
        // registered as the slot's producer so the bare `openai` vision
        // reference rewrites to it and keeps the folded credentials.
        let raw = r#"
schema_version = 2

[providers]
api_key = "global-test-key"
default_model = "vision-model"

[providers.models.openai-codex]

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.default"),
            "the globals-created default alias must resolve the bare reference"
        );
        let alias = cfg
            .providers
            .models
            .find("openai", "default")
            .expect("globals must fold into the created openai.default alias");
        assert_eq!(alias.api_key.as_deref(), Some("global-test-key"));
        assert_eq!(alias.model.as_deref(), Some("vision-model"));
    }

    #[test]
    fn v2_globals_overlay_existing_default_alias_keeps_single_producer() {
        // `default_provider` absent, global values folded onto an existing
        // `openai.default` alias. The overlay must not register a second
        // producer, or the slot would look ambiguous and the bare reference
        // would stay unrewritten.
        let raw = r#"
schema_version = 2

[providers]
api_key = "global-test-key"
default_model = "vision-model"

[providers.models.openai]
api_key = "sk-openai-test"
model = "m"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.default"),
            "overlaying globals must not make the existing default slot ambiguous"
        );
        let alias = cfg
            .providers
            .models
            .find("openai", "default")
            .expect("openai.default must exist");
        assert_eq!(
            alias.api_key.as_deref(),
            Some("sk-openai-test"),
            "per-provider api_key must win over the folded global value"
        );
    }

    #[test]
    fn v2_legacy_non_default_alias_vision_reference_migrates() {
        // `openai-codex` folds into openai as the codex alias; the reference
        // must rewrite to the non-default alias.
        let raw = r#"
schema_version = 2

[providers.models.openai-codex]
api_key = "sk-codex-test"
model = "m"

[multimodal]
vision_model_provider = "openai-codex"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.codex"),
            "legacy openai-codex reference must rewrite to openai.codex"
        );
        assert!(
            cfg.providers.models.find("openai", "codex").is_some(),
            "migrated openai-codex entry must live at openai.codex"
        );
    }

    #[test]
    fn v2_globals_created_vision_target_with_second_family_stays_bare() {
        // The maintainer's exact repro: global credentials, two migrated
        // canonical families (`openai` via openai-codex, `opencode` via
        // opencode-go), no `default_provider`, and a bare `openai` vision
        // reference. The fold must NOT claim whichever `keys().next()` family
        // iteration selects as the producer of a globals-created `default`
        // alias — nothing ties the unowned credential to it. The target stays
        // ambiguous so the bare reference is not rewritten to a slot holding a
        // credential with no stated owner.
        let raw = r#"
schema_version = 2

[providers]
api_key = "global-key"
default_model = "vision-model"

[providers.models.openai-codex]

[providers.models.opencode-go]

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai"),
            "a globals-created target across multiple families must stay bare"
        );
        let alias = cfg
            .providers
            .models
            .find("openai", "default")
            .expect("globals still fold into the first family's default alias");
        assert_eq!(alias.api_key.as_deref(), Some("global-key"));
        assert_eq!(alias.model.as_deref(), Some("vision-model"));
    }

    #[test]
    fn v2_globals_augmented_vision_target_with_second_family_stays_bare() {
        // The sibling overlay case: an existing `openai.default` producer
        // lacks a key, a second canonical family (`opencode`) exists, and no
        // `default_provider` says the global credential belongs to `openai`.
        // The globals fill the alias's key, but the slot must not be treated
        // as single-owner: a bare `openai` reference would consume a global
        // credential that has no stated owner, so the target stays ambiguous
        // and the reference stays bare.
        let raw = r#"
schema_version = 2

[providers]
api_key = "global-key"
default_model = "vision-model"

[providers.models.openai]
model = "m"

[providers.models.opencode-go]
api_key = "sk-opencode-test"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai"),
            "a globals-augmented target across multiple families must stay bare"
        );
        let alias = cfg
            .providers
            .models
            .find("openai", "default")
            .expect("openai.default must exist");
        assert_eq!(alias.api_key.as_deref(), Some("global-key"));
    }

    #[test]
    fn v2_no_op_globals_overlay_with_second_family_keeps_single_owner() {
        // The maintainer's exact finding: two canonical families with no
        // `default_provider`, and `openai.default` already complete (its own
        // api_key and model) so every global value loses to the per-provider
        // field. The fold is a no-op overlay — nothing lands on the target —
        // so it must not mark the slot ambiguous: the bare `openai` reference
        // has a stated owner (the raw `openai` entry) and must rewrite to
        // `openai.default`, preserving the per-provider credential.
        let raw = r#"
schema_version = 2

[providers]
api_key = "global-key"
default_model = "vision-model"

[providers.models.openai]
api_key = "openai-key"
model = "vision-model"

[providers.models.opencode-go]
api_key = "opencode-key"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.default"),
            "a no-op globals overlay must allow the bare reference to rewrite"
        );
        let alias = cfg
            .providers
            .models
            .find("openai", "default")
            .expect("openai.default must exist");
        assert_eq!(
            alias.api_key.as_deref(),
            Some("openai-key"),
            "the per-provider credential must win over the shadowed global"
        );
        assert_eq!(alias.model.as_deref(), Some("vision-model"));
    }

    #[test]
    fn v2_dot_bearing_legacy_vision_reference_migrates() {
        // `llama.cpp` carries a dot but is a legacy synonym for the llamacpp
        // family; the rewrite must not early-return on the dot.
        let raw = r#"
schema_version = 2

[providers.models."llama.cpp"]
uri = "http://127.0.0.1:8080/v1"
model = "m"

[multimodal]
vision_model_provider = "llama.cpp"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("llamacpp.default"),
            "dot-bearing llama.cpp reference must rewrite to llamacpp.default"
        );
        assert!(
            cfg.providers.models.find("llamacpp", "default").is_some(),
            "migrated llama.cpp entry must live at llamacpp.default"
        );
    }

    #[test]
    fn v2_bare_family_with_only_legacy_alias_left_alone() {
        // A bare `openai` reference must NOT be redirected to the `openai.codex`
        // alias created from a different legacy spelling (`openai-codex`); that
        // would silently change provider and credential selection. The bare
        // family has no `default` entry, so it stays bare (fail-closed).
        let raw = r#"
schema_version = 2

[providers.models.openai-codex]
api_key = "sk-codex-test"
model = "m"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai"),
            "bare family with only a legacy-spelling alias must stay bare"
        );
        assert!(
            cfg.providers.models.find("openai", "codex").is_some(),
            "the openai-codex entry must still migrate to openai.codex"
        );
    }

    #[test]
    fn v2_bare_family_with_only_default_alias_variant_left_alone() {
        // `qwen-intl` canonicalizes to `qwen.default` (with the intl endpoint)
        // — a DEFAULT-named alias. A bare `qwen` reference must NOT be
        // redirected to it, since that would silently inherit the qwen-intl
        // endpoint/credentials. The bare family has no own source entry, so it
        // stays bare (fail-closed).
        let raw = r#"
schema_version = 2

[providers.models.qwen-intl]
api_key = "sk-qwen-intl-test"
model = "m"

[multimodal]
vision_model_provider = "qwen"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen"),
            "bare family with only a default-named variant must stay bare"
        );
        assert!(
            cfg.providers.models.find("qwen", "default").is_some(),
            "the qwen-intl entry must still migrate to qwen.default"
        );
    }

    #[test]
    fn v2_variant_vision_reference_with_only_canonical_source_left_alone() {
        // `qwen-intl` names the international variant, but only the canonical
        // `qwen` entry exists (which migrates to qwen.default with no intl
        // endpoint). The reference must NOT be rewritten to qwen.default, since
        // that would silently drop the variant's endpoint/credentials intent.
        let raw = r#"
schema_version = 2

[providers.models.qwen]
api_key = "sk-qwen-test"
model = "m"

[multimodal]
vision_model_provider = "qwen-intl"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen-intl"),
            "variant reference with only a canonical source must stay as-is"
        );
        assert!(
            cfg.providers.models.find("qwen", "default").is_some(),
            "the canonical qwen entry must still migrate to qwen.default"
        );
    }

    #[test]
    fn v2_variant_vision_reference_with_equivalent_source_migrates() {
        // With the matching variant source present, `qwen-intl` rewrites to
        // its own migrated alias (endpoint carried on the alias entry).
        let raw = r#"
schema_version = 2

[providers.models.qwen-intl]
api_key = "sk-qwen-intl-test"
model = "m"

[multimodal]
vision_model_provider = "qwen-intl"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen.default"),
            "variant reference with an equivalent source must rewrite"
        );
    }

    #[test]
    fn v2_bare_family_with_canonical_and_legacy_alias_rewrites_to_default() {
        // A bare `openai` reference alongside BOTH a canonical `openai` entry
        // and a legacy `openai-codex` entry: the exact raw `openai` key
        // establishes the source, so the unrelated codex alias must not strand
        // the canonical reference on the configless path.
        let raw = r#"
schema_version = 2

[providers.models.openai]
api_key = "sk-openai-test"
model = "m"

[providers.models.openai-codex]
api_key = "sk-codex-test"
model = "m2"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.default"),
            "bare canonical family with its own entry must rewrite to its default alias"
        );
        assert!(
            cfg.providers.models.find("openai", "default").is_some(),
            "canonical openai entry must live at openai.default"
        );
        assert!(
            cfg.providers.models.find("openai", "codex").is_some(),
            "openai-codex entry must live at openai.codex"
        );
    }

    #[test]
    fn v2_collided_default_alias_left_bare() {
        // `qwen` and `qwen-intl` both normalize to qwen.default with different
        // endpoint variants; the retained slot is ambiguous. A bare `qwen`
        // reference must NOT be rewritten to it (it could silently pick the
        // wrong endpoint/credential), so it stays bare (fail-closed).
        let raw = r#"
schema_version = 2

[providers.models.qwen]
api_key = "sk-qwen-test"
model = "m"

[providers.models.qwen-intl]
api_key = "sk-qwen-intl-test"
model = "m2"

[multimodal]
vision_model_provider = "qwen"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen"),
            "a collided default alias slot must not capture a bare family reference"
        );
        assert!(
            cfg.providers.models.find("qwen", "default").is_some(),
            "the collided qwen.default slot still migrates"
        );
    }

    #[test]
    fn v2_synonym_collision_left_bare() {
        // `gemini` and `google` are synonyms that both collapse onto
        // gemini.default. The materialized slot retains only one of their
        // configs, so a bare `gemini` reference must not be rewritten (it could
        // silently pick the `google` config).
        let raw = r#"
schema_version = 2

[providers.models.gemini]
model = "canonical-model"
api_key = "sk-gemini"

[providers.models.google]
model = "synonym-model"
api_key = "sk-google"

[multimodal]
vision_model_provider = "gemini"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("gemini"),
            "a synonym-collided slot must not capture a bare family reference"
        );
    }

    #[test]
    fn v2_canonical_reference_with_only_legacy_synonym_source_left_bare() {
        // A bare canonical `xai` reference with only a legacy `[providers.models.grok]`
        // source: both spellings normalize to xai.default, so the retained table's
        // identity matches, but the V2 file never configured an `xai` source. The
        // canonical reference must NOT adopt the synonym's credentials — it stays
        // bare and keeps the configless path instead of silently changing which
        // source owns the vision request.
        let raw = r#"
schema_version = 2

[providers.models.grok]
api_key = "sk-grok-test"
model = "vision-model"

[multimodal]
vision_model_provider = "xai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai"),
            "canonical reference must not claim a legacy-synonym producer's alias"
        );
        assert!(
            cfg.providers.models.find("xai", "default").is_some(),
            "the grok entry still migrates to xai.default for legacy references"
        );
    }

    #[test]
    fn v2_canonical_gemini_reference_with_only_google_source_left_bare() {
        // Same ownership rule across the google->gemini synonym pair: a bare
        // `gemini` reference with only a `google` source stays on the
        // configless path rather than inheriting google's credential.
        let raw = r#"
schema_version = 2

[providers.models.google]
api_key = "sk-google-test"
model = "vision-model"

[multimodal]
vision_model_provider = "gemini"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("gemini"),
            "canonical gemini reference must not claim the google synonym's alias"
        );
    }

    #[test]
    fn v2_legacy_synonym_reference_still_rewrites_to_its_own_alias() {
        // Ownership is about the REFERENCE spelling: the legacy `grok` spelling
        // names its own migrated source, so it still rewrites to xai.default.
        let raw = r#"
schema_version = 2

[providers.models.grok]
api_key = "sk-grok-test"
model = "vision-model"

[multimodal]
vision_model_provider = "grok"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "legacy synonym reference still rewrites to its own migrated alias"
        );
    }

    #[test]
    fn v2_canonical_reference_over_explicit_default_provider_fold_rewrites() {
        // An explicit `default_provider` selector that CREATES the alias states
        // ownership of the slot even though its raw spelling differs from the
        // reference: a bare canonical `xai` reference may adopt it.
        let raw = r#"
schema_version = 2

[providers]
api_key = "sk-global-test"
default_provider = "grok"
default_model = "vision-model"

[multimodal]
vision_model_provider = "xai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "fold-created slot owned by an explicit default_provider may capture the canonical reference"
        );
    }

    #[test]
    fn v2_equivalent_explicit_overlay_over_legacy_synonym_preserves_ownership() {
        // The explicit `default_provider` selector targets a slot that
        // `alias_provider_models` already materialized from a legacy-synonym
        // model entry, and the completed alias matches the selector. The fold
        // must not register a second producer, but it must also keep the
        // selector's ownership record: the canonical-spelling vision reference
        // is otherwise left on the configless path and the migrated
        // credential-bearing alias stays unreachable.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "xai"
api_key = "sk-global-test"
default_model = "vision-model"

[providers.models.grok]
model = "grok-model"

[multimodal]
vision_model_provider = "xai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "an equivalent explicit overlay states ownership; the canonical reference must rewrite"
        );
        let alias = cfg
            .providers
            .models
            .find("xai", "default")
            .expect("migrated slot must live at xai.default");
        assert_eq!(
            alias.api_key.as_deref(),
            Some("sk-global-test"),
            "the folded global credential must be preserved on the alias"
        );
        assert_eq!(
            alias.model.as_deref(),
            Some("grok-model"),
            "the per-provider model must keep precedence over the folded global"
        );
    }

    #[test]
    fn v2_legacy_selector_overlay_preserves_ownership_for_canonical_reference() {
        // Inverse spelling of the equivalent-overlay case: the selector uses
        // the legacy synonym (`grok`) while the vision reference uses the
        // canonical family (`xai`). The overlay must keep the ownership record
        // so the canonical reference still rewrites.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "grok"
api_key = "sk-global-test"
default_model = "vision-model"

[providers.models.grok]
model = "grok-model"

[multimodal]
vision_model_provider = "xai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "a legacy-spelling selector overlay states ownership; the canonical reference must rewrite"
        );
        let alias = cfg
            .providers
            .models
            .find("xai", "default")
            .expect("migrated slot must live at xai.default");
        assert_eq!(
            alias.api_key.as_deref(),
            Some("sk-global-test"),
            "the folded global credential must be preserved on the alias"
        );
    }

    #[test]
    fn v2_colon_url_source_rewrites_bare_custom() {
        // A colon-URL source materializes custom.default with the uri. The
        // provenance records the unsplit key; the equivalence check must split
        // it back to the `custom` prefix so the bare reference rewrites.
        let raw = r#"
schema_version = 2

[providers.models."custom:https://vision.example.invalid/v1"]
model = "vision-model"

[multimodal]
vision_model_provider = "custom"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom.default"),
            "bare custom reference must rewrite to the colon-URL source's alias"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("migrated colon-URL entry must live at custom.default");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://vision.example.invalid/v1"),
            "the migrated custom.default must retain the source uri"
        );
    }

    #[test]
    fn v2_global_only_fallback_rewrites_bare_openrouter() {
        // No model entries and no default_provider: the fold synthesizes
        // openrouter.default from the global default_model. The synthesized
        // slot must be registered as a source so the bare reference rewrites.
        let raw = r#"
schema_version = 2

[providers]
default_model = "vision-model"

[multimodal]
vision_model_provider = "openrouter"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openrouter.default"),
            "bare openrouter reference must rewrite to the synthesized global alias"
        );
        assert!(
            cfg.providers.models.find("openrouter", "default").is_some(),
            "the synthesized openrouter.default must exist"
        );
    }

    #[test]
    fn v1_legacy_vision_reference_migrates_through_chain() {
        // No `schema_version` implies V1. The `model_providers` shape feeds
        // V2 `[providers.models]`, and the V2->V3 step canonicalizes the
        // legacy vision reference through the same mapping, so a V1 legacy
        // spelling must resolve too.
        let raw = r#"
[model_providers.grok]
api_key = "sk-grok-test"
model = "m"

[multimodal]
vision_model_provider = "grok"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("xai.default"),
            "V1 legacy grok reference must rewrite through the full chain"
        );
        assert!(
            cfg.providers.models.find("xai", "default").is_some(),
            "migrated V1 grok entry must live at xai.default"
        );
    }

    #[test]
    fn v2_matching_colon_url_vision_reference_rewrites_to_alias() {
        // A colon-URL reference whose full URL identity matches the sole
        // producer of the migrated alias must rewrite to the dotted alias,
        // otherwise the runtime's bare-provider construction path cannot
        // consume the typed credentials that were just migrated.
        let raw = r#"
schema_version = 2

[providers.models."custom:https://vision.example.invalid/v1"]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "custom:https://vision.example.invalid/v1"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom.default"),
            "a colon-URL reference matching its sole producer must rewrite to the alias"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("migrated colon-URL entry must live at custom.default");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://vision.example.invalid/v1"),
            "the migrated alias must retain the source uri"
        );
        assert_eq!(alias.api_key.as_deref(), Some("test-key"));
        assert_eq!(alias.model.as_deref(), Some("vision-model"));
    }

    #[test]
    fn v2_unmatched_colon_url_vision_reference_left_alone() {
        // A colon-URL reference naming a different URL than the sole producer
        // must stay unchanged (fail-closed): rewriting it would consume the
        // producer's credential against a different endpoint.
        let raw = r#"
schema_version = 2

[providers.models."custom:https://vision.example.invalid/v1"]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "custom:https://other.example.invalid/v1"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom:https://other.example.invalid/v1"),
            "a colon-URL reference whose URL differs from the sole producer must stay as-is"
        );
    }

    #[test]
    fn v2_effective_endpoint_override_matches_variant_reference() {
        // `[providers.models.qwen]` with an explicit `endpoint = "intl"`
        // override materializes qwen.default with the effective intl endpoint.
        // The `qwen-intl` reference names exactly that effective identity, so
        // the rewrite must fire even though the raw `qwen` key would normalize
        // to the cn endpoint.
        let raw = r#"
schema_version = 2

[providers.models.qwen]
api_key = "test-key"
model = "vision-model"
endpoint = "intl"

[multimodal]
vision_model_provider = "qwen-intl"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen.default"),
            "a variant reference matching the effective endpoint override must rewrite"
        );
        let alias = cfg
            .providers
            .models
            .find("qwen", "default")
            .expect("migrated qwen entry must live at qwen.default");
        assert_eq!(alias.api_key.as_deref(), Some("test-key"));
        assert_eq!(
            cfg.providers
                .models
                .qwen
                .get("default")
                .map(|c| &c.endpoint),
            Some(&crate::schema::QwenEndpoint::Intl),
            "the effective endpoint override must survive migration"
        );
    }

    #[test]
    fn v2_equivalent_bare_custom_colon_url_overlay_rewrites() {
        // A bare `[providers.models.custom]` entry whose configured `uri` is
        // already B is the SAME effective source as an explicit
        // `default_provider = "custom:https://B"` overlay. The selector must
        // not be counted as a second producer, so the bare `custom` reference
        // rewrites to the credential-bearing alias.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "custom:https://b.example.invalid/v1"
api_key = "global-key"
default_model = "vision-model"

[providers.models.custom]
uri = "https://b.example.invalid/v1"
api_key = "custom-key"
model = "vision-model"

[multimodal]
vision_model_provider = "custom"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom.default"),
            "an equivalent colon-URL overlay must not strand the bare custom reference"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("migrated custom entry must live at custom.default");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://b.example.invalid/v1"),
            "the effective uri must survive the equivalent overlay"
        );
        assert_eq!(alias.api_key.as_deref(), Some("custom-key"));
    }

    #[test]
    fn v2_bare_stepfun_with_intl_variant_stays_fail_closed() {
        // A bare `stepfun` reference must NOT be redirected to a
        // `stepfun-intl` alias that holds a different endpoint URI.
        // The variant's international URI is identity-bearing; a bare
        // family reference has no variant identity and must stay fail-closed.
        let raw = r#"
schema_version = 2

[providers.models.stepfun-intl]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "stepfun"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("stepfun"),
            "bare stepfun must stay bare when the only producer is stepfun-intl"
        );
        let alias = cfg
            .providers
            .models
            .find("stepfun", "default")
            .expect("stepfun-intl must materialize at stepfun.default");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://api.stepfun.com/intl/v1"),
            "the variant alias must retain its intl uri"
        );
    }

    #[test]
    fn v2_stepfun_intl_reference_rewrites_to_its_own_alias() {
        // `stepfun-intl` normalizes to `stepfun.default` with the intl URI.
        // A bare `stepfun-intl` reference whose variant identity matches the
        // sole producer must rewrite to the dotted alias so the migrated
        // credential and URI reach the alias-aware vision factory, while a
        // bare `stepfun` reference stays fail-closed (see the companion
        // negative test above).
        let raw = r#"
schema_version = 2

[providers.models.stepfun-intl]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "stepfun-intl"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("stepfun.default"),
            "stepfun-intl reference must rewrite to stepfun.default when it is the sole producer"
        );
        let alias = cfg
            .providers
            .models
            .find("stepfun", "default")
            .expect("stepfun-intl must materialize at stepfun.default");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://api.stepfun.com/intl/v1"),
            "the variant alias must retain its intl uri"
        );
        assert_eq!(
            alias.api_key.as_deref(),
            Some("test-key"),
            "the migrated alias must retain the credential"
        );
    }

    #[test]
    fn v2_stepfun_intl_reference_rewrites_with_trailing_slash_uri() {
        // Same as `v2_stepfun_intl_reference_rewrites_to_its_own_alias` but the
        // materialized alias carries a trailing-slash operator URI. The final
        // expected-extra equality must use slash-normalized comparison so the
        // reference still rewrites to the dotted alias.
        let raw = r#"
schema_version = 2

[providers.models.stepfun]
api_key = "test-key"
model = "vision-model"
uri = "https://api.stepfun.com/intl/v1/"

[multimodal]
vision_model_provider = "stepfun-intl"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("stepfun.default"),
            "stepfun-intl reference must rewrite to stepfun.default even when the alias URI has a trailing slash"
        );
        let alias = cfg
            .providers
            .models
            .find("stepfun", "default")
            .expect("stepfun must materialize at stepfun.default");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://api.stepfun.com/intl/v1/"),
            "the alias must retain its trailing-slash intl uri"
        );
        assert_eq!(
            alias.api_key.as_deref(),
            Some("test-key"),
            "the migrated alias must retain the credential"
        );
    }

    #[test]
    fn v2_bare_family_with_oauth_variant_stays_fail_closed() {
        // `openai-codex` adds `wire_api = responses` + `requires_openai_auth`;
        // a bare `openai` reference must not accept that variant producer.
        let raw = r#"
schema_version = 2

[providers.models.openai-codex]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai"),
            "bare openai must stay bare when the only producer is the codex variant"
        );
    }

    #[test]
    fn v2_canonical_openai_with_codex_subscription_auth_rewrites_bare_openai() {
        // Codex subscription auth is documented operator configuration on the
        // canonical `openai` slot itself (`requires_openai_auth = true`),
        // not a differently named variant: only the `openai-codex` spelling
        // materializes the separate `openai.codex` alias. A bare `openai`
        // vision reference must therefore rewrite to `openai.default`, or
        // the credential-bearing alias stays unreachable on the configless
        // path; see `v2_bare_family_with_oauth_variant_stays_fail_closed`
        // for the genuine-variant case that stays fail-closed.
        let raw = r#"
schema_version = 2

[providers.models.openai]
model = "vision-model"
requires_openai_auth = true

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.default"),
            "bare openai must rewrite to openai.default even when the canonical slot carries codex subscription auth"
        );
        let typed = cfg
            .providers
            .models
            .openai
            .get("default")
            .expect("openai.default must exist");
        assert!(
            typed.base.requires_openai_auth,
            "codex subscription auth must survive migration on the typed alias"
        );
        assert_eq!(
            cfg.providers
                .models
                .find("openai", "default")
                .and_then(|b| b.model.as_deref()),
            Some("vision-model")
        );
    }

    #[test]
    fn v2_canonical_qwen_with_oauth_rewrites_bare_qwen() {
        // A canonical `qwen` alias with an operator-selected `auth_mode =
        // "o_auth"` is still the uniquely sourced canonical credential — a
        // bare `qwen` reference must rewrite to the dotted alias so the
        // migrated OAuth configuration reaches the alias-aware vision
        // factory. This is distinct from a variant source like `qwen-code`
        // which normalizes to different extras; see
        // `v2_bare_family_with_oauth_variant_stays_fail_closed`.
        let raw = r#"
schema_version = 2

[providers.models.qwen]
model = "vision-model"
auth_mode = "o_auth"
oauth_refresh_token = "test-token"

[multimodal]
vision_model_provider = "qwen"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen.default"),
            "bare qwen must rewrite to qwen.default even when the canonical alias carries operator oauth"
        );
        let typed = cfg
            .providers
            .models
            .qwen
            .get("default")
            .expect("qwen.default must exist");
        assert_eq!(typed.auth_mode, Some(crate::schema::AuthMode::OAuth));
        assert_eq!(typed.oauth_refresh_token.as_deref(), Some("test-token"));
        assert_eq!(
            cfg.providers
                .models
                .find("qwen", "default")
                .and_then(|b| b.model.as_deref()),
            Some("vision-model")
        );
    }

    #[test]
    fn v2_canonical_openai_with_wire_api_override_rewrites_bare_openai() {
        // A canonical `openai` alias carrying an operator-selected
        // `wire_api = "responses"` is still the uniquely sourced canonical
        // credential — a bare `openai` reference must rewrite to the dotted
        // alias so the migrated key and wire protocol reach the alias-aware
        // vision factory. The only spelling whose extras include `wire_api`
        // (`openai-codex`) materializes `openai.codex`, a different alias,
        // so within this shape an unmatched `wire_api` can only be operator
        // configuration; see `v2_bare_family_with_oauth_variant_stays_fail_closed`
        // for the genuine-variant case that stays fail-closed.
        let raw = r#"
schema_version = 2

[providers.models.openai]
api_key = "test-key"
model = "vision-model"
wire_api = "responses"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.default"),
            "bare openai must rewrite to openai.default even when the canonical alias carries a wire_api override"
        );
        let typed = cfg
            .providers
            .models
            .openai
            .get("default")
            .expect("openai.default must exist");
        assert_eq!(
            typed.base.wire_api,
            Some(crate::schema::WireApi::Responses),
            "the operator wire protocol must survive migration on the typed alias"
        );
        assert_eq!(
            typed.base.api_key.as_deref(),
            Some("test-key"),
            "the credential must survive migration"
        );
        assert_eq!(
            cfg.providers
                .models
                .find("openai", "default")
                .and_then(|b| b.model.as_deref()),
            Some("vision-model")
        );
    }

    #[test]
    fn v2_legacy_qwen_oauth_variants_migrate_with_typed_auth_mode() {
        // Legacy Qwen OAuth variant spellings (`qwen-code`, `qwen-oauth`,
        // `qwen_oauth`) materialize an `auth_mode` extra. The V3 `AuthMode`
        // enum serializes snake_case, so the emitted value must be `o_auth`:
        // any other spelling fails config deserialization during migration
        // itself (`unknown variant \`oauth\``, expected `api_key` or
        // `o_auth`). Each spelling must therefore migrate successfully,
        // carry the typed OAuth mode plus the credential on the migrated
        // alias, and rewrite its own matching vision reference.
        for raw_source in ["qwen-code", "qwen-oauth", "qwen_oauth"] {
            let raw = format!(
                r#"
schema_version = 2

[providers.models.{raw_source}]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "{raw_source}"
"#
            );
            let cfg = migrate_to_current(&raw)
                .unwrap_or_else(|e| panic!("legacy variant {raw_source} must migrate: {e}"));
            assert_eq!(
                cfg.multimodal.vision_model_provider.as_deref(),
                Some("qwen.default"),
                "{raw_source} must not strand its own credential-bearing alias"
            );
            let typed = cfg
                .providers
                .models
                .qwen
                .get("default")
                .expect("migrated entry must live at qwen.default");
            assert_eq!(
                typed.auth_mode,
                Some(crate::schema::AuthMode::OAuth),
                "{raw_source} must materialize the typed V3 OAuth mode"
            );
            assert_eq!(
                typed.base.api_key.as_deref(),
                Some("test-key"),
                "the credential must survive migration"
            );
        }
    }

    #[test]
    fn v2_legacy_minimax_oauth_variants_migrate_with_typed_auth_mode() {
        // Same contract as the Qwen OAuth variants, for the MiniMax OAuth
        // spellings (`minimax-oauth`, `minimax-oauth-global`,
        // `minimax-oauth-cn`): the emitted `auth_mode` must deserialize as
        // the typed V3 `AuthMode::OAuth`, and each spelling's own vision
        // reference must reach its migrated credential-bearing alias.
        for raw_source in ["minimax-oauth", "minimax-oauth-global", "minimax-oauth-cn"] {
            let raw = format!(
                r#"
schema_version = 2

[providers.models.{raw_source}]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "{raw_source}"
"#
            );
            let cfg = migrate_to_current(&raw)
                .unwrap_or_else(|e| panic!("legacy variant {raw_source} must migrate: {e}"));
            assert_eq!(
                cfg.multimodal.vision_model_provider.as_deref(),
                Some("minimax.default"),
                "{raw_source} must not strand its own credential-bearing alias"
            );
            let typed = cfg
                .providers
                .models
                .minimax
                .get("default")
                .expect("migrated entry must live at minimax.default");
            assert_eq!(
                typed.auth_mode,
                Some(crate::schema::AuthMode::OAuth),
                "{raw_source} must materialize the typed V3 OAuth mode"
            );
            assert_eq!(
                typed.base.api_key.as_deref(),
                Some("test-key"),
                "the credential must survive migration"
            );
        }
    }

    #[test]
    fn v2_bare_qwen_with_oauth_variant_producer_stays_fail_closed() {
        // A bare `qwen` reference names only the canonical cn-endpoint
        // identity; a `qwen-code` producer carries code-endpoint plus OAuth
        // identity the reference did not name, so the reference must stay
        // bare rather than adopt that variant's credentials.
        let raw = r#"
schema_version = 2

[providers.models.qwen-code]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "qwen"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("qwen"),
            "bare qwen must stay bare when the only producer is the qwen-code oauth variant"
        );
        let typed = cfg
            .providers
            .models
            .qwen
            .get("default")
            .expect("qwen-code must materialize at qwen.default");
        assert_eq!(
            typed.auth_mode,
            Some(crate::schema::AuthMode::OAuth),
            "the variant alias itself must still migrate with typed OAuth mode"
        );
    }

    #[test]
    fn v2_empty_global_extra_headers_with_second_family_keeps_single_owner() {
        // An empty `extra_headers = {}` is a semantic no-op and must not
        // claim alias ownership across multiple families. The existing
        // per-provider alias should remain reachable.
        let raw = r#"
schema_version = 2

[providers]
api_key = "global-test-key"
default_model = "vision-model"
extra_headers = {}

[providers.models.openai]
api_key = "openai-test-key"
model = "vision-model"

[providers.models.opencode-go]
api_key = "other-test-key"
model = "other-model"

[multimodal]
vision_model_provider = "openai"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("openai.default"),
            "empty extra_headers must not strand a valid keyed vision alias"
        );
        let alias = cfg
            .providers
            .models
            .find("openai", "default")
            .expect("openai.default must exist");
        assert_eq!(alias.api_key.as_deref(), Some("openai-test-key"));
    }

    #[test]
    fn v2_matching_colon_url_with_global_api_path_rewrites() {
        // A colon-URL `default_provider` with a global `api_path` is
        // materialized as `uri = base + path`. The vision reference that
        // names the same effective source (base + path) must rewrite to the
        // dotted alias; the `api_path` composition is handled at the fold
        // site where the selector's URL is composed before the equivalence
        // check, so the rewrite requires exact normalized equality.
        let raw = r#"
schema_version = 2

[providers]
default_provider = "custom:https://vision.example.invalid"
api_path = "/v1"
api_key = "test-key"
default_model = "vision-model"

[multimodal]
vision_model_provider = "custom:https://vision.example.invalid/v1"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom.default"),
            "matching colon-URL with api_path must rewrite to the dotted alias"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("custom.default must exist");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://vision.example.invalid/v1"),
            "the composed URI must survive as base + api_path"
        );
        assert_eq!(alias.api_key.as_deref(), Some("test-key"));
    }

    #[test]
    fn v2_colon_url_with_different_path_stays_fail_closed() {
        // A sole `custom:https://.../v2` producer and a base-only reference
        // with no `api_path` must remain fail-closed. The previous permissive
        // prefix check (`base` matches any `/v2` descendant) would have
        // incorrectly rewritten this distinct endpoint.
        let raw = r#"
schema_version = 2

[providers.models."custom:https://vision.example.invalid/v2"]
api_key = "test-key"
model = "vision-model"

[multimodal]
vision_model_provider = "custom:https://vision.example.invalid"
"#;
        let cfg = migrate_to_current(raw).unwrap();
        assert_eq!(
            cfg.multimodal.vision_model_provider.as_deref(),
            Some("custom:https://vision.example.invalid"),
            "a base-only reference must not match a distinct /v2 endpoint with no api_path"
        );
        let alias = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("custom.default must exist");
        assert_eq!(
            alias.uri.as_deref(),
            Some("https://vision.example.invalid/v2"),
            "the distinct /v2 URI must be retained"
        );
    }

    #[test]
    fn provider_pruner_never_panics_on_non_table_shapes() {
        // Array-of-tables where a family map is expected, scalar [providers],
        // array alias value. The salvage path is the daemon's never-fail
        // loader, and prune_bad_provider_aliases carries expect() calls that
        // rely on the scalar pre-passes; pin that invariant here.
        for raw in [
            "schema_version = 4\nproviders = 3\n",
            "schema_version = 4\n[[providers.models.ollama]]\nmodel = \"x\"\n",
            "schema_version = 4\n[providers.models.ollama]\nai = [1, 2]\n",
            "schema_version = 4\n[providers.models]\nollama = [1]\n",
        ] {
            let _ = migrate_to_current_salvaged(raw);
        }
    }

    #[test]
    fn scalar_provider_nodes_pruned_without_sinking_section() {
        // A scalar where a family/kind table is required must drop only
        // that node, not the whole [providers] section.
        let raw = r#"
schema_version = 4

[providers.models]
ollama = "oops"

[providers.models.custom.rag_bot]
uri = "http://localhost:8000/v1"
model = "m"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert_eq!(load.dropped, vec!["providers.models.ollama"]);
        assert!(
            load.config
                .providers
                .models
                .find("custom", "rag_bot")
                .is_some(),
            "valid alias must survive a scalar sibling family"
        );
    }

    #[test]
    fn valid_alias_survives_broken_sibling() {
        let raw = r#"
schema_version = 4

[channels.email.broken]
enabled = true
smtp_host = "smtp.example.com"
username = "u"
password = "p"
from_address = "a@example.com"

[channels.email.good]
enabled = true
imap_host = "imap.example.com"
smtp_host = "smtp.example.com"
username = "u"
password = "p"
from_address = "a@example.com"
"#;
        let cfg = migrate_to_current_resilient(raw);
        assert!(
            cfg.channels.email.contains_key("good"),
            "valid sibling must be kept"
        );
        assert!(
            !cfg.channels.email.contains_key("broken"),
            "invalid sibling must be pruned"
        );
    }

    #[test]
    fn broken_non_channel_section_falls_back_to_default() {
        // A type mismatch outside the channel maps must NOT abort the daemon:
        // the section is dropped to its default so the operator can repair it.
        let raw = r#"
schema_version = 4

[heartbeat]
enabled = "not-a-bool"
"#;
        let cfg = migrate_to_current_resilient(raw);
        // `[heartbeat]` reverted to its serde default; load did not panic.
        assert!(!cfg.heartbeat.enabled);
        assert_eq!(cfg.heartbeat.interval_minutes, 30);
    }

    #[test]
    fn unparseable_config_falls_back_to_defaults() {
        // Not even valid TOML — the daemon still boots on defaults so the
        // operator can reach a repair surface and overwrite the file.
        let cfg = migrate_to_current_resilient("this is not valid TOML {{{");
        assert_eq!(cfg.schema_version, Config::default().schema_version);
    }

    #[test]
    fn future_schema_version_falls_back_to_defaults() {
        // A schema newer than this binary can't be migrated, but the daemon
        // must still start rather than refuse to boot.
        let raw = format!("schema_version = {}\n", CURRENT_SCHEMA_VERSION + 100);
        let cfg = migrate_to_current_resilient(&raw);
        assert_eq!(cfg.schema_version, Config::default().schema_version);
    }

    #[test]
    fn unparseable_config_marks_whole_config_degraded() {
        // Whole-config loss loses every security-critical section at once, so it
        // must mark the posture degraded — otherwise the serving gate has no
        // signal and boots a defaulted security posture silently.
        let load = migrate_to_current_salvaged("this is not valid TOML {{{");
        assert!(
            load.dropped_security
                .iter()
                .any(|p| p == WHOLE_CONFIG_SENTINEL),
            "unparseable config must degrade security posture, got {:?}",
            load.dropped_security
        );
    }

    #[test]
    fn future_schema_version_marks_whole_config_degraded() {
        let raw = format!("schema_version = {}\n", CURRENT_SCHEMA_VERSION + 100);
        let load = migrate_to_current_salvaged(&raw);
        assert!(
            load.dropped_security
                .iter()
                .any(|p| p == WHOLE_CONFIG_SENTINEL),
            "unsupported future schema must degrade security posture, got {:?}",
            load.dropped_security
        );
    }

    #[test]
    fn unsalvageable_root_marks_whole_config_degraded() {
        // A root that is not a table cannot be salvaged section-by-section; the
        // final deserialize fallback defaults the whole config and must mark it.
        let raw = "schema_version = 4\nthis_is_a_bare_top_level = \"value\"\n[\n";
        let load = migrate_to_current_salvaged(raw);
        assert!(
            !load.dropped_security.is_empty(),
            "an unsalvageable root must degrade security posture, got {:?}",
            load.dropped_security
        );
    }

    #[test]
    fn strict_path_still_errors_for_tooling() {
        // `migrate_to_current` stays strict — repair tooling needs the error.
        let raw = r#"
schema_version = 4

[channels.email.fakeemail]
enabled = true
smtp_host = "smtp.example.com"
username = "u"
password = "p"
from_address = "a@example.com"
"#;
        assert!(
            migrate_to_current(raw).is_err(),
            "strict path must surface the defect for repair tooling"
        );
    }

    #[test]
    fn broken_users_roster_is_reported_as_security_degraded() {
        // A malformed [users] roster salvaged to an empty roster is
        // indistinguishable from an intentional no-roster config, which
        // re-opens the shared-operator fallback. It must surface as a
        // security-critical drop so exposure gating can react.
        let raw = r#"
schema_version = 4

[users.alice]
uid = "not-an-integer"
permission_profiles = ["operator"]
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.dropped_security.iter().any(|p| p == "users"),
            "malformed [users] must be a security-critical drop, got: {:?}",
            load.dropped_security
        );
    }

    #[test]
    fn broken_security_section_is_reported_as_degraded() {
        let raw = r#"
schema_version = 4

[security]
audit = "should-be-a-table-not-a-string"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.dropped_security.iter().any(|p| p == "security"),
            "malformed [security] must be reported as a security-critical drop"
        );
        assert!(
            load.dropped.is_empty(),
            "security drop must not also appear in the plain dropped list"
        );
    }

    #[test]
    fn broken_non_security_section_is_plain_drop_not_security() {
        let raw = r#"
schema_version = 4

[heartbeat]
enabled = "not-a-bool"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.dropped.iter().any(|p| p == "heartbeat"),
            "malformed [heartbeat] must be a plain drop"
        );
        assert!(
            load.dropped_security.is_empty(),
            "a non-security section must never be flagged security-critical"
        );
    }

    #[test]
    fn broken_channel_type_block_is_dropped_not_fatal() {
        let raw = r#"
schema_version = 4

[channels]
email = "oops-this-should-be-a-table"

[channels.telegram.main]
enabled = true
bot_token = "t"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.dropped.iter().any(|p| p == "channels.email"),
            "the broken whole-type block must be dropped, got {:?}",
            load.dropped
        );
        assert!(
            load.config.channels.telegram.contains_key("main"),
            "valid sibling channel type must survive a broken-type drop"
        );
    }

    #[test]
    fn multiple_independent_bad_sections_all_dropped() {
        let raw = r#"
schema_version = 4

[heartbeat]
enabled = "not-a-bool"

[backup]
enabled = "also-not-a-bool"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.dropped.iter().any(|p| p == "heartbeat"),
            "first offender must be dropped, got {:?}",
            load.dropped
        );
        assert!(
            load.dropped.iter().any(|p| p == "backup"),
            "second offender must be dropped, got {:?}",
            load.dropped
        );
    }

    #[test]
    fn multiple_bad_sections_one_security_critical() {
        let raw = r#"
schema_version = 4

[security]
audit = "should-be-a-table-not-a-string"

[heartbeat]
enabled = "not-a-bool"
"#;
        let load = migrate_to_current_salvaged(raw);
        assert!(
            load.dropped_security.iter().any(|p| p == "security"),
            "malformed [security] must be classified security-critical, got {:?}",
            load.dropped_security
        );
        assert!(
            load.dropped.iter().any(|p| p == "heartbeat"),
            "malformed [heartbeat] must be a plain drop, got {:?}",
            load.dropped
        );
        assert!(
            !load.dropped.iter().any(|p| p == "security"),
            "security drop must not also appear in the plain dropped list"
        );
    }

    // ── migrate_file_in_place atomic-write semantics ──
    fn setup_temp_config_dir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("temp dir")
    }

    #[test]
    fn migrate_file_in_place_writes_backup_and_replaces_atomically() {
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        // Minimal V1 input (no schema_version) so migration runs.
        std::fs::write(&path, "default_model_provider = \"openai\"\nfoo = 1\n").unwrap();

        let report = migrate_file_in_place(&path)
            .expect("migration succeeds")
            .expect("migration ran (V1 input)");

        // Backup retains the original content verbatim.
        let backup = std::fs::read_to_string(&report.backup_path).unwrap();
        assert!(
            backup.contains("default_model_provider = \"openai\"") && backup.contains("foo = 1"),
            "backup must contain the original V1 content; got: {backup}"
        );

        // Original is replaced with migrated content.
        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(
            migrated.contains("schema_version"),
            "migrated config must carry a schema_version line; got: {migrated}"
        );

        // No `<file>.tmp-*` files left behind in the parent.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".config.toml.tmp-")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp files must remain after a successful migration; got {leftovers:?}"
        );
    }

    #[test]
    fn migrate_file_in_place_noop_when_already_current() {
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!("schema_version = {CURRENT_SCHEMA_VERSION}\n"),
        )
        .unwrap();

        let report = migrate_file_in_place(&path).expect("idempotent on current schema");
        assert!(
            report.is_none(),
            "no migration should run when the file is already at CURRENT_SCHEMA_VERSION"
        );
        // No backup file should exist when the migration didn't run.
        let backup = path.with_file_name("config.toml.backup");
        assert!(
            !backup.exists(),
            "no `.backup` should be created on the no-op path; got {}",
            backup.display()
        );
    }
    const V3_WITH_NEVIS: &str = r#"
schema_version = 3

[security]
trust_daemon_uid = false

[security.nevis]
enabled = true
instance_url = "https://nevis.example.com"
client_secret = "plaintext-nevis-secret"
"#;

    #[test]
    fn v3_to_v4_removes_security_nevis_and_reports_it() {
        let (migrated, notices) = migrate_file_with_notices(V3_WITH_NEVIS)
            .unwrap()
            .expect("a V3 config migrates to V4");

        let value: toml::Value = toml::from_str(&migrated).unwrap();
        assert_eq!(detect_version(&value).unwrap(), 4);
        let security = value["security"].as_table().expect("[security] kept");
        assert!(!security.contains_key("nevis"), "{migrated}");
        assert_eq!(
            security.get("trust_daemon_uid"),
            Some(&toml::Value::Boolean(false)),
            "sibling keys are untouched"
        );
        assert!(
            !migrated.contains("plaintext-nevis-secret"),
            "the retired table's secret must not survive on disk"
        );
        assert_eq!(
            notices,
            vec![MigrationNotice::Removed {
                path: "security.nevis".to_string(),
                reason: RETIRED_KEYS[0].reason,
            }]
        );
    }

    #[test]
    fn migration_notices_never_carry_retired_values() {
        let (_, notices) = migrate_file_with_notices(V3_WITH_NEVIS).unwrap().unwrap();
        for notice in &notices {
            let rendered = format!(
                "{} {}",
                notice.message(),
                serde_json::to_string(notice).unwrap()
            );
            assert!(!rendered.contains("plaintext-nevis-secret"), "{rendered}");
            assert!(!rendered.contains("nevis.example.com"), "{rendered}");
        }
    }

    #[test]
    fn v3_without_retired_keys_migrates_silently() {
        let (migrated, notices) = migrate_file_with_notices("schema_version = 3\n")
            .unwrap()
            .expect("a V3 config is stamped V4");
        assert_eq!(
            detect_version(&toml::from_str(&migrated).unwrap()).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert!(notices.is_empty(), "{notices:?}");
    }

    #[test]
    fn a_retired_key_in_a_current_config_is_dropped_with_a_notice() {
        let raw = format!(
            "schema_version = {CURRENT_SCHEMA_VERSION}\n\n[security]\ntrust_daemon_uid = false\n\n\
             [security.nevis]\nclient_secret = \"plaintext-nevis-secret\"\n"
        );
        let (migrated, notices) = migrate_file_with_notices(&raw)
            .unwrap()
            .expect("a current config holding a retired key is rewritten");
        assert!(!migrated.contains("nevis"), "{migrated}");
        assert!(!migrated.contains("plaintext-nevis-secret"));
        assert!(migrated.contains("trust_daemon_uid = false"));
        assert!(matches!(
            notices.as_slice(),
            [MigrationNotice::Removed { path, .. }] if path == "security.nevis"
        ));

        let load = migrate_to_current_salvaged(&raw);
        assert!(!load.config.security.trust_daemon_uid);
        assert!(
            load.dropped_security.is_empty(),
            "a retired key must not degrade the security section: {:?}",
            load.dropped_security
        );
        assert_eq!(load.notices, notices);
    }

    #[test]
    fn current_config_is_left_alone() {
        assert_eq!(
            migrate_file_with_notices(&format!("schema_version = {CURRENT_SCHEMA_VERSION}\n"))
                .unwrap(),
            None
        );
    }

    #[test]
    fn missing_schema_version_reports_the_v1_assumption() {
        let (_, notices) = migrate_file_with_notices("foo = 1\n")
            .unwrap()
            .expect("an unversioned config is migrated from V1");
        assert_eq!(notices.first(), Some(&MigrationNotice::AssumedV1));

        let (_, notices) = migrate_file_with_notices("schema_version = 1\nfoo = 1\n")
            .unwrap()
            .unwrap();
        assert!(
            !notices.contains(&MigrationNotice::AssumedV1),
            "an explicit V1 is not an assumption"
        );
    }

    #[test]
    fn resilient_load_carries_migration_notices() {
        let load = migrate_to_current_salvaged(V3_WITH_NEVIS);
        assert!(!load.config.security.trust_daemon_uid);
        assert!(
            load.dropped_security.is_empty(),
            "{:?}",
            load.dropped_security
        );
        assert!(
            load.notices.iter().any(
                |n| matches!(n, MigrationNotice::Removed { path, .. } if path == "security.nevis")
            ),
            "{:?}",
            load.notices
        );

        let unversioned = migrate_to_current_salvaged("");
        assert_eq!(
            unversioned.notices.first(),
            Some(&MigrationNotice::AssumedV1)
        );
    }

    // ── A missing `schema_version` on a plainly V3 file ─────────────

    /// The case that lost a contributor's channels: a hand-written V3 file
    /// with alias-keyed sections and no `schema_version`. It used to be read
    /// as V1 and migrated, which collapsed `work`/`home` into `default`.
    const HAND_WRITTEN_V3_WITHOUT_VERSION: &str = r#"
[providers.models.ollama.default]
model = "llama3"

[channels.discord.work]
enabled = true
bot_token = "work-token"
mention_only = true

[channels.discord.home]
enabled = true
bot_token = "home-token"
mention_only = false
"#;

    #[test]
    fn a_plainly_v3_file_without_the_key_is_read_as_v3() {
        let v: toml::Value = toml::from_str(HAND_WRITTEN_V3_WITHOUT_VERSION).unwrap();
        assert_eq!(detect_version(&v).unwrap(), INFERRED_SCHEMA_VERSION);
        assert_ne!(
            INFERRED_SCHEMA_VERSION, CURRENT_SCHEMA_VERSION,
            "read as the current version, the file would skip the V3 -> V4 step"
        );
    }

    #[test]
    fn an_inferred_v3_file_is_migrated_to_v4_and_keeps_every_channel_alias() {
        let config =
            migrate_to_current(HAND_WRITTEN_V3_WITHOUT_VERSION).expect("a V3-shaped file loads");
        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        let discord = &config.channels.discord;
        assert!(discord.get("work").is_some_and(|work| work.mention_only));
        assert!(discord.get("home").is_some_and(|home| !home.mention_only));
        assert!(
            !discord.contains_key("default"),
            "nothing may be collapsed into a synthesized `default` alias"
        );

        let (migrated, notices) = migrate_file_with_notices(HAND_WRITTEN_V3_WITHOUT_VERSION)
            .unwrap()
            .expect("an inferred V3 file is carried to V4");
        assert_eq!(notices, vec![MigrationNotice::InferredV3]);
        assert_eq!(
            detect_version(&toml::from_str(&migrated).unwrap()).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert!(
            migrated.contains("[channels.discord.work]")
                && migrated.contains("[channels.discord.home]"),
            "{migrated}"
        );

        let load = migrate_to_current_salvaged(HAND_WRITTEN_V3_WITHOUT_VERSION);
        assert_eq!(load.notices, vec![MigrationNotice::InferredV3]);
        assert!(load.dropped.is_empty() && load.dropped_security.is_empty());
    }

    #[test]
    fn an_inferred_v3_file_still_loses_its_retired_keys() {
        let raw = format!(
            "{HAND_WRITTEN_V3_WITHOUT_VERSION}\n[agents.default]\nmax_tool_iterations = 7\n"
        );
        let (migrated, notices) = migrate_file_with_notices(&raw).unwrap().unwrap();
        assert_eq!(notices.first(), Some(&MigrationNotice::InferredV3));
        assert!(notices.iter().any(|n| matches!(
            n,
            MigrationNotice::Removed { path, .. } if path == "agents.default.max_tool_iterations"
        )));
        assert!(!migrated.contains("max_tool_iterations"), "{migrated}");
        assert!(
            !notices.contains(&MigrationNotice::AssumedV1),
            "a V3 reading is not the V1 assumption"
        );
    }

    #[test]
    fn an_explicit_version_is_never_reported_as_inferred() {
        let raw = format!("schema_version = 3\n{HAND_WRITTEN_V3_WITHOUT_VERSION}");
        let v: toml::Value = toml::from_str(&raw).unwrap();
        assert_eq!(detect_version(&v).unwrap(), 3);
        let (_, notices) = migrate_file_with_notices(&raw).unwrap().unwrap();
        assert!(
            !notices.contains(&MigrationNotice::InferredV3)
                && !notices.contains(&MigrationNotice::AssumedV1),
            "{notices:?}"
        );
    }

    #[test]
    fn a_v2_shaped_file_without_the_key_keeps_the_v1_reading() {
        // V2 held fields directly on the family and channel sections.
        for raw in [
            "[providers.models.ollama]\nmodel = \"llama3\"\n",
            "[channels.discord]\nbot_token = \"t\"\nenabled = true\n",
            // One alias-keyed section does not outvote a flat one.
            "[providers.models.ollama.default]\nmodel = \"llama3\"\n\n[channels.discord]\nbot_token = \"t\"\n",
        ] {
            let v: toml::Value = toml::from_str(raw).unwrap();
            assert_eq!(detect_version(&v).unwrap(), 1, "{raw}");
            let (_, notices) = migrate_file_with_notices(raw).unwrap().unwrap();
            assert_eq!(notices.first(), Some(&MigrationNotice::AssumedV1), "{raw}");
        }
    }

    #[test]
    fn a_v1_only_key_keeps_the_v1_reading() {
        let raw = format!("default_model = \"gpt-4\"\n{HAND_WRITTEN_V3_WITHOUT_VERSION}");
        let v: toml::Value = toml::from_str(&raw).unwrap();
        assert_eq!(detect_version(&v).unwrap(), 1);
    }

    #[test]
    fn keys_shared_with_the_current_schema_do_not_veto_the_inference() {
        let raw =
            format!("[cron.nightly]\nschedule = \"0 0 * * *\"\n{HAND_WRITTEN_V3_WITHOUT_VERSION}");
        let v: toml::Value = toml::from_str(&raw).unwrap();
        assert_eq!(detect_version(&v).unwrap(), INFERRED_SCHEMA_VERSION);
    }

    #[test]
    fn a_file_with_no_alias_keyed_section_keeps_the_v1_reading() {
        for raw in [
            "",
            "[gateway]\nport = 42617\n",
            "[providers.models.ollama]\n",
        ] {
            let v: toml::Value = toml::from_str(raw).unwrap();
            assert_eq!(detect_version(&v).unwrap(), 1, "{raw:?}");
        }
    }

    #[test]
    fn the_bundled_v1_fixture_is_still_read_as_v1() {
        let v: toml::Value = toml::from_str(V1_FIXTURE).unwrap();
        assert_eq!(detect_version(&v).unwrap(), 1);
    }

    /// A V2 provider profile holding only a map-valued field has the shape of
    /// a V3 family with one alias. Neither reading is safe, so the version is
    /// required rather than guessed; stated, each reading keeps its data.
    #[test]
    fn a_map_only_v2_provider_profile_requires_an_explicit_version() {
        let raw = "[providers.models.openai.extra_headers]\nX-Trace = \"keep\"\n";
        let err = detect_version(&toml::from_str(raw).unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("[providers.models.openai.extra_headers]")
                && err.contains("schema_version = 2")
                && err.contains("schema_version = 3"),
            "{err}"
        );
        assert!(migrate_file_with_notices(raw).is_err());

        // Stated as V2, the headers stay on the migrated `default` alias.
        let value: toml::Value = toml::from_str(&format!("schema_version = 2\n{raw}")).unwrap();
        let Migrated { value, .. } = migrate_toml(value).unwrap().unwrap();
        assert_eq!(
            value["providers"]["models"]["openai"]["default"]["extra_headers"]["X-Trace"].as_str(),
            Some("keep"),
            "{value:#?}"
        );

        // Stated as V3, `extra_headers` is the operator's own alias.
        let value: toml::Value = toml::from_str(&format!("schema_version = 3\n{raw}")).unwrap();
        let Migrated { value, .. } = migrate_toml(value).unwrap().unwrap();
        assert!(
            value["providers"]["models"]["openai"]
                .get("extra_headers")
                .is_some(),
            "{value:#?}"
        );
    }

    /// The same holds for a channel whose only content is a map-valued field.
    #[test]
    fn a_map_only_v2_channel_requires_an_explicit_version() {
        let raw = "[channels.git.events.push]\nmessage = true\n";
        let err = detect_version(&toml::from_str(raw).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("[channels.git.events]"), "{err}");
    }

    /// An ordinary V2 profile holds scalar fields beside its maps, so it is
    /// plainly not the V3 shape and keeps the V1 reading, as before.
    #[test]
    fn an_ordinary_v2_profile_with_a_map_field_keeps_the_v1_reading() {
        let raw = "[providers.models.openai]\nmodel = \"gpt-4o\"\n\n\
                   [providers.models.openai.extra_headers]\nX-Trace = \"keep\"\n";
        assert_eq!(detect_version(&toml::from_str(raw).unwrap()).unwrap(), 1);
    }

    /// A V3 alias named like a map-valued field of a V2 entry reads both ways
    /// whatever it holds, so the version is required: whether its content
    /// happens to fit the field is not evidence of which version was meant.
    /// Stated, it is the operator's alias. A table named like a scalar field
    /// cannot be that field, so it stays an alias.
    #[test]
    fn a_v3_alias_named_like_a_map_field_requires_an_explicit_version() {
        for raw in [
            "[providers.models.openai.extra_headers]\nmodel = \"gpt-4o\"\n",
            "[providers.models.openai.pricing]\nmodel = \"gpt-4o\"\n",
        ] {
            assert!(
                detect_version(&toml::from_str(raw).unwrap()).is_err(),
                "{raw}"
            );
            let stated = format!("schema_version = 3\n{raw}");
            assert_eq!(
                detect_version(&toml::from_str(&stated).unwrap()).unwrap(),
                3
            );
        }
        let scalar_named = "[providers.models.openai.model]\nmodel = \"gpt-4o\"\n";
        assert_eq!(
            detect_version(&toml::from_str(scalar_named).unwrap()).unwrap(),
            INFERRED_SCHEMA_VERSION
        );
    }

    /// An empty map-valued field serializes to nothing, so it cannot be told
    /// from its own round trip; the probe fills it in and still sees a field.
    #[test]
    fn an_empty_map_field_still_makes_the_shape_ambiguous() {
        for raw in [
            "[providers.models.openai.extra_headers]\n",
            "[providers.models.openai.pricing]\n",
        ] {
            let err = detect_version(&toml::from_str(raw).unwrap())
                .unwrap_err()
                .to_string();
            assert!(err.contains("schema_version = 2"), "{raw}: {err}");
        }
    }

    /// V2-only settings in an otherwise V3-shaped file: read as V3 they would
    /// be ignored, read as V1 the V3 sections would be reshaped, so the
    /// version is required.
    #[test]
    fn a_v2_only_setting_beside_v3_sections_requires_an_explicit_version() {
        for (raw, named) in [
            (
                "[providers]\napi_key = \"sk-global\"\n\n[providers.models.openai.default]\nmodel = \"gpt-4o\"\n",
                "providers.api_key",
            ),
            (
                "[providers.models.openai.default]\nmodel = \"gpt-4o\"\n\n[agents.coder]\nallowed_tools = [\"shell\"]\n",
                "agents.coder.allowed_tools",
            ),
        ] {
            let err = detect_version(&toml::from_str(raw).unwrap())
                .unwrap_err()
                .to_string();
            assert!(err.contains(named), "{err}");
        }
        // An inert V3 tunable is not a V2 marker.
        let raw = format!(
            "{HAND_WRITTEN_V3_WITHOUT_VERSION}\n[agents.default]\nmax_tool_iterations = 7\n"
        );
        assert_eq!(
            detect_version(&toml::from_str(&raw).unwrap()).unwrap(),
            INFERRED_SCHEMA_VERSION
        );
    }

    /// Every V2-only agent marker must be a key no V3 agent reads, or a V3
    /// file using it would be refused as ambiguous.
    #[test]
    fn v2_only_agent_keys_are_not_v3_agent_fields() {
        for key in V2_ONLY_AGENT_KEYS {
            for value in [
                toml::Value::String("x".into()),
                toml::Value::Integer(2),
                toml::Value::Float(0.5),
                toml::Value::Boolean(true),
                toml::Value::Array(vec![toml::Value::String("x".into())]),
            ] {
                let mut agent = toml::Table::new();
                agent.insert((*key).to_string(), value);
                let Ok(parsed) =
                    toml::Value::Table(agent).try_into::<crate::schema::AliasedAgentConfig>()
                else {
                    continue;
                };
                let round_tripped = toml::Value::try_from(parsed).unwrap();
                assert!(
                    round_tripped.get(*key).is_none(),
                    "`{key}` is a V3 agent field and cannot mark a file as V2"
                );
            }
        }
    }

    /// A `schema_version` past `u32` is refused as newer than the binary, not
    /// narrowed: `4294967300` would otherwise wrap to 4.
    #[test]
    fn a_schema_version_beyond_u32_is_refused_not_wrapped() {
        let wrapped = "schema_version = 4294967300\n\n[security.nevis]\nenabled = true\n";
        let err = detect_version(&toml::from_str(wrapped).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("newer than this binary supports"), "{err}");
        assert!(migrate_file_with_notices(wrapped).is_err());

        let max = format!("schema_version = {}\n", u32::MAX);
        assert_eq!(
            detect_version(&toml::from_str(&max).unwrap()).unwrap(),
            u32::MAX
        );
        assert!(migrate_file_with_notices(&max).is_err());
    }

    /// Every top-level section the current schema has, including keyed
    /// sections that are empty by default.
    fn current_top_level_sections() -> std::collections::BTreeSet<String> {
        Config::default()
            .prop_fields()
            .iter()
            .filter_map(|field| field.name.split('.').next().map(str::to_string))
            .chain(
                Config::map_key_sections()
                    .iter()
                    .filter_map(|section| section.path.split('.').next().map(str::to_string)),
            )
            .chain(
                toml::Value::try_from(Config::default())
                    .expect("serialize default config")
                    .as_table()
                    .expect("config is a table")
                    .keys()
                    .cloned(),
            )
            .collect()
    }

    /// `V1_KEYS_STILL_CURRENT` must name exactly the V1 legacy keys that are
    /// also top-level sections today: missing one would let a current file
    /// that uses it be read as V1; listing a V1-only key would let a V1 file
    /// be read as V3.
    #[test]
    fn v1_keys_still_current_are_exactly_the_current_top_level_sections() {
        let current = current_top_level_sections();
        let shared: Vec<&str> = V1_LEGACY_KEYS
            .iter()
            .copied()
            .filter(|key| current.contains(*key))
            .collect();
        assert_eq!(shared, V1_KEYS_STILL_CURRENT);
    }

    // ── V4 retirements of keys no schema field reads ────────────────

    /// A retirement must never name a key the current schema still reads:
    /// that would silently delete working configuration. Top-level removals
    /// must not be current sections, and a retired channel type must not be a
    /// live channel type.
    #[test]
    fn a_retirement_never_names_a_key_the_schema_still_reads() {
        let sections = current_top_level_sections();
        for key in RETIRED_KEYS {
            if let ([section], Retirement::Remove) = (key.path, key.retirement) {
                assert!(
                    *section == ANY_KEY || !sections.contains(*section),
                    "`{section}` is a current top-level section and cannot be retired"
                );
            }
            if matches!(key.retirement, Retirement::RemoveChannel) {
                let channel_type = retired_channel_type(key)
                    .unwrap_or_else(|| panic!("{:?}: must be [\"channels\", <type>]", key.path));
                assert!(
                    !crate::schema::v2::V3_CHANNEL_TYPES.contains(&channel_type),
                    "`{channel_type}` is a live channel type and cannot be retired"
                );
            }
        }
    }

    #[test]
    fn v3_to_v4_retires_top_level_twitter_and_reddit_but_not_the_channels() {
        let raw = r#"schema_version = 3

[twitter]
bearer_token = "TWITTER-SENTINEL"

[reddit]
client_secret = "REDDIT-SENTINEL"

[channels.twitter.main]
enabled = true
"#;
        let (migrated, notices) = migrate_file_with_notices(raw).unwrap().unwrap();
        assert_eq!(removed_paths(&notices), vec!["reddit", "twitter"]);
        assert!(!migrated.contains("SENTINEL"), "{migrated}");
        assert!(
            migrated.contains("[channels.twitter.main]"),
            "the Twitter channel itself is not retired: {migrated}"
        );

        let value: toml::Value = toml::from_str(raw).unwrap();
        let Migrated { value, notices } = migrate_toml(value).unwrap().unwrap();
        assert_eq!(removed_paths(&notices), vec!["reddit", "twitter"]);
        let root = value.as_table().unwrap();
        assert!(!root.contains_key("twitter") && !root.contains_key("reddit"));
        assert!(root["channels"].get("twitter").is_some());
    }

    /// A V3 file that holds `[channels.notion]`, agents naming it and a peer
    /// group bound to it.
    const V3_WITH_NOTION_CHANNEL: &str = r#"schema_version = 3

[channels.notion.main]
token = "NOTION-SENTINEL"

# operator comment kept by the in-place migration
[channels.telegram.main]
bot_token = "telegram-token"

[agents.default]
channels = ["notion.main", "telegram.main", "notion"]

[agents.other]
channels = ["telegram.main"]

[escalation]
alert_channels = [" notion.main", "telegram.main"]

[peer_groups.notion_team]
channel = "notion.main"
agents = ["default"]

[peer_groups.telegram_team]
channel = "telegram"
agents = ["default"]
"#;

    /// What retiring the notion channel from [`V3_WITH_NOTION_CHANNEL`] must
    /// report, in a stable order.
    fn notion_retirement_report(notices: &[MigrationNotice]) -> Vec<String> {
        let mut report: Vec<String> = notices
            .iter()
            .map(|notice| match notice {
                MigrationNotice::Removed { path, .. } => format!("removed {path}"),
                MigrationNotice::ReferenceRemoved {
                    path, reference, ..
                } => format!("pruned {} from {path}", reference.trim()),
                MigrationNotice::ReferenceKept {
                    path, reference, ..
                } => format!("kept {} in {path}", reference.trim()),
                other => panic!("unexpected notice {other:?}"),
            })
            .collect();
        report.sort();
        report
    }

    /// The agent bindings and the padded escalation entry are dropped; the
    /// peer group is a hard reference, so it is kept and reported, as the
    /// `alias_refs` cascade refuses to delete it.
    const NOTION_RETIREMENT_REPORT: &[&str] = &[
        "kept notion.main in peer_groups.notion_team.channel",
        "pruned notion from agents.default.channels",
        "pruned notion.main from agents.default.channels",
        "pruned notion.main from escalation.alert_channels",
        "removed channels.notion",
    ];

    #[test]
    fn v3_to_v4_retires_the_notion_channel_with_every_reference_to_it() {
        // The parsed-value path (load and the typed chain).
        let value: toml::Value = toml::from_str(V3_WITH_NOTION_CHANNEL).unwrap();
        let Migrated { value, notices } = migrate_toml(value).unwrap().unwrap();
        assert_eq!(notion_retirement_report(&notices), NOTION_RETIREMENT_REPORT);
        let root = value.as_table().unwrap();
        assert!(root["channels"].get("notion").is_none());
        assert!(root["channels"].get("telegram").is_some());
        assert_eq!(
            root["agents"]["default"]["channels"],
            toml::Value::Array(vec!["telegram.main".into()])
        );
        assert_eq!(
            root["agents"]["other"]["channels"],
            toml::Value::Array(vec!["telegram.main".into()]),
            "an agent with no notion reference is untouched"
        );
        assert_eq!(
            root["escalation"]["alert_channels"],
            toml::Value::Array(vec!["telegram.main".into()]),
            "a padded reference is matched after trimming"
        );
        assert_eq!(
            root["peer_groups"]["notion_team"]["channel"].as_str(),
            Some("notion.main"),
            "a peer group bound to the retired channel is kept for the operator"
        );
        assert!(root["peer_groups"].get("telegram_team").is_some());

        // The in-place document path (`config migrate` on a V3+ file).
        let (migrated, notices) = migrate_file_with_notices(V3_WITH_NOTION_CHANNEL)
            .unwrap()
            .unwrap();
        assert_eq!(notion_retirement_report(&notices), NOTION_RETIREMENT_REPORT);
        assert!(!migrated.contains("NOTION-SENTINEL"), "{migrated}");
        assert!(migrated.contains("telegram_team"), "{migrated}");
        assert!(
            migrated.contains("channels = [\"telegram.main\"]"),
            "a pruned list keeps a clean spelling: {migrated}"
        );
        assert!(
            migrated.contains("# operator comment kept by the in-place migration"),
            "{migrated}"
        );
        let reparsed: toml::Value = toml::from_str(&migrated).unwrap();
        assert_eq!(
            reparsed["agents"]["default"]["channels"],
            toml::Value::Array(vec!["telegram.main".into()])
        );
        assert_eq!(detect_version(&reparsed).unwrap(), CURRENT_SCHEMA_VERSION);
    }

    /// References to a retired channel are pruned on every load and save of a
    /// current file, not only by the migration step, and in every table
    /// spelling: dotted keys and inline tables as well as headers. Here the
    /// `[channels.notion]` section itself is already gone.
    #[test]
    fn notion_references_are_pruned_from_a_current_file_in_every_spelling() {
        let raw = r#"schema_version = 4
agents.default.channels = ["notion.work", "telegram.main"]
peer_groups = { notion_team = { channel = "notion" }, keep = { channel = "telegram.main" } }
"#;
        let (out, notices) = apply_doc(raw);
        assert_eq!(
            notion_retirement_report(&notices),
            vec![
                "kept notion in peer_groups.notion_team.channel",
                "pruned notion.work from agents.default.channels",
            ]
        );
        let reparsed: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(
            reparsed["agents"]["default"]["channels"],
            toml::Value::Array(vec!["telegram.main".into()])
        );
        assert!(reparsed["peer_groups"].get("notion_team").is_some());
        assert!(reparsed["peer_groups"].get("keep").is_some());

        let value: toml::Value = toml::from_str(raw).unwrap();
        let Migrated { notices, .. } = migrate_toml(value)
            .unwrap()
            .expect("a current file holding retired references is cleaned on load");
        assert_eq!(notion_retirement_report(&notices).len(), 2);
    }

    /// Retiring a channel must not empty the last binding any agent has: with
    /// no binding anywhere, the runtime hands every configured channel to the
    /// fallback agent. The entry is kept and reported instead, and a current
    /// file holding only such kept references is not rewritten.
    #[test]
    fn retiring_a_channel_keeps_the_last_agent_binding() {
        let raw = r#"schema_version = 3

[channels.telegram.ops]
bot_token = "telegram-token"

[agents.social]
channels = ["notion.main"]
"#;
        let value: toml::Value = toml::from_str(raw).unwrap();
        let Migrated { value, notices } = migrate_toml(value).unwrap().unwrap();
        assert_eq!(
            value["agents"]["social"]["channels"],
            toml::Value::Array(vec!["notion.main".into()])
        );
        assert!(notices.iter().any(|n| matches!(
            n,
            MigrationNotice::ReferenceKept { reason, .. } if *reason == KEPT_LAST_BINDING
        )));

        let (migrated, _) = migrate_file_with_notices(raw).unwrap().unwrap();
        assert!(
            migrated.contains("channels = [\"notion.main\"]"),
            "{migrated}"
        );

        let current = raw.replace("schema_version = 3", "schema_version = 4");
        assert_eq!(
            migrate_file_with_notices(&current).unwrap(),
            None,
            "a kept reference alone is not a change to write"
        );
    }

    /// The reference sites pruned here are the ones the canonical
    /// `alias_refs` cascade knows for a channel, with the same strength: the
    /// lists are soft (entries dropped), the peer-group binding is hard
    /// (never deleted). The fixture references a channel alias at every site.
    #[test]
    fn channel_reference_sites_match_the_alias_cascade() {
        let cfg: Config = toml::from_str(
            r#"
[channels.telegram.probe]
bot_token = "t"

[agents.a]
channels = ["telegram.probe"]

[escalation]
alert_channels = ["telegram.probe"]

[peer_groups.g]
channel = "telegram.probe"
"#,
        )
        .unwrap();
        let sites = crate::alias_refs::find_all_references(
            &cfg,
            &crate::alias_refs::AliasKind::Channel {
                channel_type: "telegram".into(),
            },
            "probe",
        );
        let mut from_cascade: Vec<(String, bool)> = sites
            .iter()
            .map(|site| {
                let path = site.path.split('[').next().unwrap_or(&site.path);
                let mut segments: Vec<&str> = path.split('.').collect();
                if matches!(segments.first(), Some(&"agents" | &"peer_groups")) {
                    segments[1] = ANY_KEY;
                }
                (
                    segments.join("."),
                    matches!(site.strength, crate::alias_refs::RefStrength::Hard),
                )
            })
            .collect();
        from_cascade.sort();
        from_cascade.dedup();
        let mut ours: Vec<(String, bool)> = CHANNEL_REFERENCE_LISTS
            .iter()
            .map(|path| (path.join("."), false))
            .chain(
                CHANNEL_REFERENCE_BINDINGS
                    .iter()
                    .map(|path| (path.join("."), true)),
            )
            .collect();
        ours.sort();
        assert_eq!(from_cascade, ours);
    }

    /// `config migrate` keeps the pre-upgrade original once per version: a
    /// later retirement-only migration replaces the `.backup` slot but not
    /// the versioned copy.
    #[test]
    fn config_migrate_keeps_the_pre_upgrade_original() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let original = "schema_version = 3\n\n[security.nevis]\nenabled = true\n";
        std::fs::write(&path, original).unwrap();
        migrate_file_in_place(&path)
            .unwrap()
            .expect("a V3 file migrates");
        let versioned = dir.path().join("config.toml.v3.backup");
        assert_eq!(std::fs::read_to_string(&versioned).unwrap(), original);

        // A retired key pasted back into the V4 file: the next migration is
        // retirement-only and replaces `.backup`, not the versioned copy.
        let mut current = std::fs::read_to_string(&path).unwrap();
        current.push_str("\n[security.nevis]\nenabled = true\n");
        std::fs::write(&path, &current).unwrap();
        migrate_file_in_place(&path)
            .unwrap()
            .expect("the retired key is removed");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml.backup")).unwrap(),
            current
        );
        assert_eq!(std::fs::read_to_string(&versioned).unwrap(), original);
    }

    #[test]
    fn a_retired_key_is_recognized_at_and_under_its_path() {
        let summary = [
            "runtime_profiles",
            "fast",
            "context_compression",
            "summary_model",
        ];
        assert!(retired_key_covering(&summary).is_some());
        assert!(retired_key_covering(&["security", "nevis", "client_secret"]).is_some());
        assert!(retired_key_covering(&["channels", "notion", "main", "token"]).is_some());
        assert!(
            retired_key_covering(&["runtime_profiles", "fast", "max_tool_iterations"]).is_none()
        );
        assert!(retired_key_covering(&["security"]).is_none());
    }

    #[test]
    fn every_retired_key_names_a_migrated_version() {
        for key in RETIRED_KEYS {
            assert!(
                (2..=CURRENT_SCHEMA_VERSION).contains(&key.retired_in),
                "{:?} must be retired by a step the chain runs",
                key.path
            );
            assert!(!key.path.is_empty() && !key.reason.is_empty(), "{key:?}");
            if let Retirement::Rename { to } = key.retirement {
                let wildcards = |p: &[&str]| p.iter().filter(|s| **s == ANY_KEY).count();
                assert!(
                    wildcards(to) <= wildcards(key.path),
                    "{:?}: a rename target cannot use more wildcards than its source",
                    key.path
                );
            }
        }
    }

    #[test]
    fn v3_to_v4_retires_inert_tunables_in_every_agent_and_profile() {
        let raw = r#"
schema_version = 3

[agents.coder]
runtime_profile = "fast"
max_tool_iterations = 40
parallel_tools = true

[agents.writer]
compact_context = true

[agents.plain]
enabled = true

[runtime_profiles.fast]
max_tool_iterations = 12

[runtime_profiles.fast.context_compression]
summary_model = "haiku"
threshold_ratio = 0.5

[runtime_profiles.slow.context_compression]
summary_model = "opus"
"#;
        let (migrated, notices) = migrate_file_with_notices(raw).unwrap().unwrap();
        let value: toml::Value = toml::from_str(&migrated).unwrap();

        let coder = value["agents"]["coder"].as_table().unwrap();
        assert!(
            !coder.contains_key("max_tool_iterations") && !coder.contains_key("parallel_tools")
        );
        assert_eq!(
            coder["runtime_profile"].as_str(),
            Some("fast"),
            "live keys stay"
        );
        assert!(
            !value["agents"]["writer"]
                .as_table()
                .unwrap()
                .contains_key("compact_context")
        );
        assert_eq!(value["agents"]["plain"]["enabled"].as_bool(), Some(true));
        assert_eq!(
            value["runtime_profiles"]["fast"]["max_tool_iterations"].as_integer(),
            Some(12),
            "the runtime-profile copy of a tunable is the live one and is kept"
        );
        let fast_cc = value["runtime_profiles"]["fast"]["context_compression"]
            .as_table()
            .unwrap();
        assert!(!fast_cc.contains_key("summary_model"), "{fast_cc:?}");
        assert_eq!(fast_cc["threshold_ratio"].as_float(), Some(0.5));
        let slow = value["runtime_profiles"]["slow"].as_table().unwrap();
        assert!(
            !slow.contains_key("context_compression"),
            "a literal container left empty is dropped: {slow:?}"
        );

        let mut removed: Vec<&str> = notices
            .iter()
            .filter_map(|n| match n {
                MigrationNotice::Removed { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        removed.sort_unstable();
        assert_eq!(
            removed,
            [
                "agents.coder.max_tool_iterations",
                "agents.coder.parallel_tools",
                "agents.writer.compact_context",
                "runtime_profiles.fast.context_compression.summary_model",
                "runtime_profiles.slow.context_compression.summary_model",
            ],
            "each concrete match is reported under its real path"
        );
    }

    #[test]
    fn a_wildcard_does_not_descend_into_non_tables() {
        let raw = "schema_version = 3\nagents = \"not-a-table\"\n";
        let (migrated, notices) = migrate_file_with_notices(raw).unwrap().unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        assert!(migrated.contains("agents = \"not-a-table\""));
    }

    const TEST_RENAMES: &[RetiredKey] = &[
        RetiredKey {
            retired_in: 9,
            path: &["old", "knob"],
            retirement: Retirement::Rename {
                to: &["new", "section", "knob"],
            },
            reason: "renamed for the test",
        },
        RetiredKey {
            retired_in: 8,
            path: &["other"],
            retirement: Retirement::Remove,
            reason: "a different version",
        },
        RetiredKey {
            retired_in: 7,
            path: &["bots", ANY_KEY, "old_limit"],
            retirement: Retirement::Rename {
                to: &["bots", ANY_KEY, "limits", "max"],
            },
            reason: "wildcard rename for the test",
        },
    ];

    fn apply(raw: &str, version: u32) -> (toml::Value, Vec<MigrationNotice>) {
        let mut value: toml::Value = toml::from_str(raw).unwrap();
        let mut notices = Vec::new();
        apply_retired_keys(&mut value, version, TEST_RENAMES, &mut notices);
        (value, notices)
    }

    #[test]
    fn a_retired_rename_moves_the_value_and_creates_parents() {
        let (value, notices) = apply("other = 1\n[old]\nknob = 7\nkeep = true\n", 9);
        assert_eq!(value["new"]["section"]["knob"].as_integer(), Some(7));
        assert!(value["old"].get("knob").is_none());
        assert_eq!(value["old"]["keep"].as_bool(), Some(true));
        assert_eq!(
            value["other"].as_integer(),
            Some(1),
            "only version 9 entries apply"
        );
        assert_eq!(
            notices,
            vec![MigrationNotice::Renamed {
                from: "old.knob".to_string(),
                to: "new.section.knob".to_string(),
                reason: "renamed for the test",
            }]
        );
    }

    #[test]
    fn a_retired_rename_never_overwrites_the_replacement() {
        let (value, notices) = apply("[old]\nknob = 7\n[new.section]\nknob = 1\n", 9);
        assert_eq!(value["new"]["section"]["knob"].as_integer(), Some(1));
        assert!(
            value.get("old").is_none(),
            "a literal container left empty is dropped"
        );
        assert!(matches!(
            notices.as_slice(),
            [MigrationNotice::RenameConflict { from, to, .. }] if from == "old.knob" && to == "new.section.knob"
        ));
    }

    #[test]
    fn a_blocked_rename_creates_no_parent_tables() {
        let (value, notices) = apply("new = \"scalar\"\n[old]\nknob = 7\n", 9);
        assert_eq!(value["new"].as_str(), Some("scalar"));
        assert!(matches!(
            notices.as_slice(),
            [MigrationNotice::RenameConflict { .. }]
        ));

        let mut root: toml::Table = toml::from_str("[new]\nx = 1\n").unwrap();
        assert!(!put_path_if_vacant(
            &mut root,
            &["new", "x", "deeper"],
            toml::Value::Integer(2)
        ));
        assert_eq!(toml::Value::Table(root)["new"]["x"].as_integer(), Some(1));
    }

    #[test]
    fn a_wildcard_rename_keeps_each_value_under_its_own_key() {
        let (value, notices) = apply(
            "[bots.a]\nold_limit = 1\n[bots.b]\nold_limit = 2\n[bots.b.limits]\nmax = 9\n[bots.c]\nkeep = 3\n",
            7,
        );
        assert_eq!(value["bots"]["a"]["limits"]["max"].as_integer(), Some(1));
        assert_eq!(
            value["bots"]["b"]["limits"]["max"].as_integer(),
            Some(9),
            "an existing replacement wins"
        );
        assert!(value["bots"]["a"].get("old_limit").is_none());
        assert!(value["bots"]["b"].get("old_limit").is_none());
        assert_eq!(value["bots"]["c"]["keep"].as_integer(), Some(3));
        assert!(
            value["bots"]["c"].get("limits").is_none(),
            "no match, no new table"
        );
        assert!(notices.contains(&MigrationNotice::Renamed {
            from: "bots.a.old_limit".to_string(),
            to: "bots.a.limits.max".to_string(),
            reason: "wildcard rename for the test",
        }));
        assert!(notices.contains(&MigrationNotice::RenameConflict {
            from: "bots.b.old_limit".to_string(),
            to: "bots.b.limits.max".to_string(),
            reason: "wildcard rename for the test",
        }));
        assert_eq!(notices.len(), 2);
    }

    #[test]
    fn an_absent_retired_key_changes_nothing() {
        let (value, notices) = apply("[old]\nkeep = true\n", 9);
        assert!(notices.is_empty());
        assert!(value.get("new").is_none());
        let (_, notices) = apply("[old]\nknob = { nested = 1 }\n", 8);
        assert!(
            notices.is_empty(),
            "version 9 entries do not apply at version 8"
        );
    }

    fn apply_doc(raw: &str) -> (String, Vec<MigrationNotice>) {
        let mut doc: toml_edit::DocumentMut = raw.parse().unwrap();
        let notices = apply_retired_keys_to_doc(doc.as_table_mut());
        (doc.to_string(), notices)
    }

    fn removed_paths(notices: &[MigrationNotice]) -> Vec<&str> {
        let mut paths: Vec<&str> = notices
            .iter()
            .map(|notice| match notice {
                MigrationNotice::Removed { path, .. } => path.as_str(),
                other => panic!("only removals expected, got {other:?}"),
            })
            .collect();
        paths.sort_unstable();
        paths
    }

    #[test]
    fn doc_cleanup_removes_retired_nevis_in_every_spelling() {
        // A `[security.nevis]` header next to a populated `[security]`.
        let (out, notices) = apply_doc(
            "# keep me\n[security]\ntrust_daemon_uid = false\n\n[security.nevis]\nclient_secret = \"s\"\n",
        );
        assert_eq!(removed_paths(&notices), ["security.nevis"]);
        assert!(
            !out.contains("nevis") && !out.contains("client_secret"),
            "{out}"
        );
        assert!(
            out.contains("# keep me") && out.contains("trust_daemon_uid = false"),
            "{out}"
        );

        // Dotted keys at the root.
        let (out, notices) = apply_doc("security.nevis.enabled = true\nlocale = \"en\"\n");
        assert_eq!(removed_paths(&notices), ["security.nevis"]);
        assert_eq!(out, "locale = \"en\"\n");

        // Inline tables, with a sibling that stays.
        let (out, notices) =
            apply_doc("security = { nevis = { enabled = true }, trust_daemon_uid = false }\n");
        assert_eq!(removed_paths(&notices), ["security.nevis"]);
        assert!(
            !out.contains("nevis") && out.contains("trust_daemon_uid = false"),
            "{out}"
        );

        // A `[security]` left empty by the removal is dropped, whether it was
        // implicit or had its own header.
        for raw in [
            "[security.nevis]\nenabled = true\n",
            "[security]\n\n[security.nevis]\nenabled = true\n",
        ] {
            let (out, notices) = apply_doc(raw);
            assert_eq!(removed_paths(&notices), ["security.nevis"]);
            assert!(!out.contains("security"), "{raw:?} -> {out:?}");
        }
    }

    #[test]
    fn doc_cleanup_uses_the_same_wildcard_entries_as_migration() {
        let raw = "[agents.coder]\nmax_tool_iterations = 40\n\n\
                   [agents.writer]\nruntime_profile = \"fast\"\nparallel_tools = true\n\n\
                   [runtime_profiles.fast]\nmax_tool_iterations = 12\n\n\
                   [runtime_profiles.fast.context_compression]\nsummary_model = \"haiku\"\n";
        let (out, notices) = apply_doc(raw);
        assert_eq!(
            removed_paths(&notices),
            [
                "agents.coder.max_tool_iterations",
                "agents.writer.parallel_tools",
                "runtime_profiles.fast.context_compression.summary_model",
            ]
        );
        let value: toml::Value = toml::from_str(&out).unwrap();
        assert!(
            value["agents"]["coder"].as_table().unwrap().is_empty(),
            "an operator-named alias is kept even when its only key was retired: {out}"
        );
        assert_eq!(
            value["agents"]["writer"]["runtime_profile"].as_str(),
            Some("fast")
        );
        assert_eq!(
            value["runtime_profiles"]["fast"]["max_tool_iterations"].as_integer(),
            Some(12)
        );
        assert!(
            value["runtime_profiles"]["fast"]
                .get("context_compression")
                .is_none(),
            "a literal container left empty is dropped: {out}"
        );

        // The same entries, through the migration path, report the same paths.
        let (_, migration_notices) =
            migrate_file_with_notices(&format!("schema_version = {CURRENT_SCHEMA_VERSION}\n{raw}"))
                .unwrap()
                .unwrap();
        assert_eq!(removed_paths(&migration_notices), removed_paths(&notices));
    }

    #[test]
    fn doc_cleanup_leaves_everything_else_byte_for_byte() {
        let untouched = "schema_version = 4\n\n\
                         # Operator note.\n\
                         [channels.telegram.main]\n\
                         bot_token = \"enc:v1:UNRELATED-CIPHERTEXT\"   # trailing comment\n";
        let (out, notices) = apply_doc(untouched);
        assert!(notices.is_empty());
        assert_eq!(
            out, untouched,
            "a document without retired keys is not reformatted"
        );

        let with_retired = format!("{untouched}\n[security.nevis]\nclient_secret = \"x\"\n");
        let (out, notices) = apply_doc(&with_retired);
        assert_eq!(removed_paths(&notices), ["security.nevis"]);
        assert!(
            out.starts_with(untouched.trim_end()),
            "unrelated lines are preserved byte for byte:\n{out}"
        );
        let (again, notices) = apply_doc(&out);
        assert!(notices.is_empty());
        assert_eq!(again, out, "a second cleanup changes nothing");
    }

    #[test]
    fn doc_cleanup_applies_wildcard_renames_without_overwriting() {
        let mut doc: toml_edit::DocumentMut =
            "[bots.a]\nold_limit = 1\n\n[bots.b]\nold_limit = 2\n\n[bots.b.limits]\nmax = 9\n"
                .parse()
                .unwrap();
        let mut notices = Vec::new();
        apply_retired_keys_to_doc_table(doc.as_table_mut(), 7, TEST_RENAMES, &mut notices);
        let value: toml::Value = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(value["bots"]["a"]["limits"]["max"].as_integer(), Some(1));
        assert_eq!(value["bots"]["b"]["limits"]["max"].as_integer(), Some(9));
        assert!(value["bots"]["a"].get("old_limit").is_none());
        assert!(value["bots"]["b"].get("old_limit").is_none());
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().any(|n| matches!(n, MigrationNotice::RenameConflict { from, .. } if from == "bots.b.old_limit")));
    }

    #[test]
    fn an_alias_emptied_by_retirement_stays_in_the_written_file() {
        // The profile's only content is the retired key, and an agent refers to
        // it. Dropping the implicit `slow` table on write would leave that
        // reference dangling.
        let raw = format!(
            "schema_version = {CURRENT_SCHEMA_VERSION}\n\n\
             [agents.coder]\nruntime_profile = \"slow\"\n\n\
             [runtime_profiles.slow.context_compression]\nsummary_model = \"haiku\"\n"
        );
        let (migrated, notices) = migrate_file_with_notices(&raw).unwrap().unwrap();
        assert_eq!(
            removed_paths(&notices),
            ["runtime_profiles.slow.context_compression.summary_model"]
        );
        let value: toml::Value = toml::from_str(&migrated).unwrap();
        assert!(
            value["runtime_profiles"]["slow"]
                .as_table()
                .unwrap()
                .is_empty(),
            "the alias survives, empty: {migrated}"
        );
        let load = migrate_to_current_salvaged(&migrated);
        assert!(load.config.runtime_profiles.contains_key("slow"));
        assert!(load.dropped.is_empty() && load.dropped_security.is_empty());

        let (out, notices) = apply_doc(&raw);
        assert_eq!(
            removed_paths(&notices),
            ["runtime_profiles.slow.context_compression.summary_model"]
        );
        let value: toml::Value = toml::from_str(&out).unwrap();
        assert!(
            value["runtime_profiles"]["slow"]
                .as_table()
                .unwrap()
                .is_empty(),
            "the document cleanup keeps it too: {out}"
        );
    }

    // ── F1: an alias emptied by retirement survives every TOML spelling ──

    /// Configs whose aliases (`slow`, `worker`) hold only retired content,
    /// spelled in the ways `toml_edit` would otherwise drop once emptied.
    const DOTTED_ALIAS_CONFIGS: &[(&str, &str)] = &[
        (
            "dotted keys at the root",
            "schema_version = 4\n\
             runtime_profiles.slow.context_compression.summary_model = \"haiku\"\n\
             agents.worker.compact_context = true\n\n\
             [agents.coder]\nruntime_profile = \"slow\"\n",
        ),
        (
            "dotted keys inside an inline table",
            "schema_version = 4\n\
             runtime_profiles = { slow.context_compression.summary_model = \"haiku\" }\n\
             agents = { worker.compact_context = true, coder = { runtime_profile = \"slow\" } }\n",
        ),
        (
            "nested inline tables",
            "schema_version = 4\n\
             runtime_profiles = { slow = { context_compression = { summary_model = \"haiku\" } } }\n\
             agents = { worker = { compact_context = true }, coder = { runtime_profile = \"slow\" } }\n",
        ),
        (
            "dotted keys under a header",
            "schema_version = 4\n\n\
             [runtime_profiles]\nslow.context_compression.summary_model = \"haiku\"\n\n\
             [agents]\nworker.compact_context = true\ncoder.runtime_profile = \"slow\"\n",
        ),
    ];

    fn assert_aliases_survive(case: &str, written: &str) {
        let value: toml::Value = toml::from_str(written)
            .unwrap_or_else(|e| panic!("{case}: output must stay valid TOML ({e}):\n{written}"));
        assert!(
            value["runtime_profiles"]["slow"]
                .as_table()
                .is_some_and(toml::Table::is_empty),
            "{case}: the referenced profile alias must survive, empty:\n{written}"
        );
        assert!(
            value["agents"]["worker"]
                .as_table()
                .is_some_and(toml::Table::is_empty),
            "{case}: the agent alias must survive, empty:\n{written}"
        );
        assert_eq!(
            value["agents"]["coder"]["runtime_profile"].as_str(),
            Some("slow")
        );
        assert!(!written.contains("summary_model") && !written.contains("compact_context"));
        let load = migrate_to_current_salvaged(written);
        assert!(
            load.config.runtime_profiles.contains_key("slow")
                && load.config.agents.contains_key("worker"),
            "{case}: the aliases must survive a reload"
        );
        assert!(load.notices.is_empty(), "{case}: {:?}", load.notices);
        assert!(load.dropped.is_empty() && load.dropped_security.is_empty());
    }

    #[test]
    fn doc_cleanup_keeps_aliases_in_every_toml_spelling() {
        for (case, raw) in DOTTED_ALIAS_CONFIGS {
            let (out, notices) = apply_doc(raw);
            assert_eq!(
                removed_paths(&notices),
                [
                    "agents.worker.compact_context",
                    "runtime_profiles.slow.context_compression.summary_model",
                ],
                "{case}"
            );
            assert_aliases_survive(case, &out);
        }
    }

    #[test]
    fn config_migrate_keeps_aliases_in_every_toml_spelling() {
        for (case, raw) in DOTTED_ALIAS_CONFIGS {
            let (migrated, _) = migrate_file_with_notices(raw)
                .unwrap()
                .unwrap_or_else(|| panic!("{case}: retired keys must trigger a rewrite"));
            assert_aliases_survive(case, &migrated);
        }
    }

    #[test]
    fn structural_migration_keeps_an_emptied_dotted_alias_rendered() {
        // The V1/V2 route rebuilds the document and then walks each notice's
        // parents; an empty dotted or implicit alias must still be written.
        let mut doc: toml_edit::DocumentMut =
            "a.slow.x = 1\n[b]\nc = { worker.y = 2 }\n".parse().unwrap();
        doc["a"]["slow"].as_table_like_mut().unwrap().remove("x");
        doc["b"]["c"]["worker"]
            .as_table_like_mut()
            .unwrap()
            .remove("y");
        let notices = [
            MigrationNotice::Removed {
                path: "a.slow.x".to_string(),
                reason: "test",
            },
            MigrationNotice::Removed {
                path: "b.c.worker.y".to_string(),
                reason: "test",
            },
        ];
        keep_emptied_tables_visible(doc.as_table_mut(), &notices);
        let value: toml::Value = toml::from_str(&doc.to_string()).unwrap();
        assert!(
            value["a"]["slow"]
                .as_table()
                .is_some_and(toml::Table::is_empty),
            "{doc}"
        );
        assert!(
            value["b"]["c"]["worker"]
                .as_table()
                .is_some_and(toml::Table::is_empty),
            "{doc}"
        );
    }

    // ── F2: retirements that used to live outside the table ──

    #[test]
    fn node_transport_and_wati_are_retired_on_every_path() {
        let retired = "\n[node_transport]\nenabled = true\nshared_secret = \"NODE-SENTINEL\"\n\n\
                       [channels.wati.production]\nenabled = true\napi_token = \"WATI-SENTINEL\"\n\n\
                       [channels_config.wati]\napi_token = \"LEGACY-WATI-SENTINEL\"\n";
        let expected = ["channels.wati", "channels_config.wati", "node_transport"];
        for version in [3, CURRENT_SCHEMA_VERSION] {
            let raw = format!("schema_version = {version}\nlocale = \"en\"\n{retired}");
            let (migrated, notices) = migrate_file_with_notices(&raw)
                .unwrap()
                .unwrap_or_else(|| panic!("V{version}: retired sections must trigger a rewrite"));
            assert_eq!(removed_paths(&notices), expected, "V{version}");
            for sentinel in ["NODE-SENTINEL", "WATI-SENTINEL", "node_transport", "wati"] {
                assert!(
                    !migrated.contains(sentinel),
                    "V{version} kept {sentinel}:\n{migrated}"
                );
            }
            let load = migrate_to_current_salvaged(&raw);
            assert_eq!(removed_paths(&load.notices), expected, "V{version} load");

            let (out, notices) = apply_doc(&raw);
            assert_eq!(
                removed_paths(&notices),
                expected,
                "V{version} document cleanup"
            );
            assert!(!out.contains("SENTINEL"), "{out}");
        }

        // A legacy V1 file is renamed by the V1 step, then retired.
        let (migrated, notices) = migrate_file_with_notices(
            "[channels_config.wati]\napi_token = \"LEGACY-WATI-SENTINEL\"\n",
        )
        .unwrap()
        .unwrap();
        assert!(notices.contains(&MigrationNotice::AssumedV1));
        assert!(notices.iter().any(
            |n| matches!(n, MigrationNotice::Removed { path, .. } if path == "channels.wati")
        ));
        assert!(!migrated.contains("SENTINEL"), "{migrated}");
    }

    // ── F3: `config migrate` from V3 or later edits the file in place ──

    const UNTOUCHED_V3_BODY: &str = "locale = 'en'  # operator note: keep this explanation\n\
         \n\
         [gateway]\n\
         port = 42_617 # underscores stay\n\
         host = \"127.0.0.1\"\n\
         \n\
         # A comment block before a section.\n\
         [cost]\n\
         daily_limit_usd = 1e1\n\
         warn_at_percent = 0x50\n";

    #[test]
    fn v3_migration_changes_only_the_version_line() {
        let raw =
            format!("schema_version = 3  # stamped by an older zeroclaw\n{UNTOUCHED_V3_BODY}");
        let (migrated, notices) = migrate_file_with_notices(&raw).unwrap().unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(
            migrated,
            format!("schema_version = 4  # stamped by an older zeroclaw\n{UNTOUCHED_V3_BODY}"),
            "everything but the version must keep its exact bytes"
        );
    }

    #[test]
    fn retirement_migration_keeps_untouched_bytes() {
        let retired = "\n[security.nevis]\nclient_secret = 'RETIRED_SENTINEL'\n";
        for version in [3, CURRENT_SCHEMA_VERSION] {
            let raw = format!("schema_version = {version}\n{UNTOUCHED_V3_BODY}{retired}");
            let (migrated, notices) = migrate_file_with_notices(&raw).unwrap().unwrap();
            assert_eq!(removed_paths(&notices), ["security.nevis"], "V{version}");
            assert_eq!(
                migrated,
                format!("schema_version = {CURRENT_SCHEMA_VERSION}\n{UNTOUCHED_V3_BODY}"),
                "V{version}: only the retired table and the version may change"
            );
        }
    }

    // ── R1: the dashboard's retired pairing settings ──

    const RETIRED_CODE_LENGTH_BODY: &str = "locale = \"en\"\n\n\
         [gateway.pairing_dashboard]\n\
         code_length = 8\n\
         code_ttl_secs = 3600\n\n\
         [gateway.pairing_code]\n\
         length = 20\n\
         charset = \"unambiguous\"\n";

    /// The table as V3 wrote it: the length the V3 step retired goes first,
    /// then the rest of the table at V4.
    #[test]
    fn retired_dashboard_code_length_is_removed_on_every_path() {
        for version in [3, CURRENT_SCHEMA_VERSION] {
            let raw = format!("schema_version = {version}\n{RETIRED_CODE_LENGTH_BODY}");
            let expected = [
                "gateway.pairing_dashboard",
                "gateway.pairing_dashboard.code_length",
            ];

            let (migrated, notices) = migrate_file_with_notices(&raw)
                .unwrap()
                .unwrap_or_else(|| panic!("V{version}: the retired key must trigger a rewrite"));
            assert_eq!(removed_paths(&notices), expected, "V{version}");
            assert_eq!(
                migrated,
                format!(
                    "schema_version = {CURRENT_SCHEMA_VERSION}\n{}",
                    RETIRED_CODE_LENGTH_BODY.replace(
                        "[gateway.pairing_dashboard]\ncode_length = 8\ncode_ttl_secs = 3600\n\n",
                        ""
                    )
                ),
                "V{version}: only the retired table and the version may change"
            );

            let load = migrate_to_current_salvaged(&raw);
            assert_eq!(removed_paths(&load.notices), expected, "V{version} load");
            assert_eq!(
                load.config.gateway.pairing_code.length, 20,
                "the live policy stays"
            );

            let (out, notices) = apply_doc(&raw);
            assert_eq!(
                removed_paths(&notices),
                expected,
                "V{version} document cleanup"
            );
            assert!(!out.contains("pairing_dashboard") && out.contains("length = 20"));
        }
    }

    /// The four settings V4 retires with the table, after a body whose bytes
    /// must not change.
    const RETIRED_DASHBOARD_TABLE: &str = "\n[gateway.pairing_dashboard]\n\
         code_ttl_secs = 3600\n\
         max_pending_codes = 3\n\
         max_failed_attempts = 5\n\
         lockout_secs = 300\n";

    #[test]
    fn retired_dashboard_table_is_removed_with_one_notice_and_no_other_change() {
        let expected = vec![MigrationNotice::Removed {
            path: "gateway.pairing_dashboard".to_string(),
            reason: retired_key_covering(&["gateway", "pairing_dashboard", "lockout_secs"])
                .expect("the dashboard table is retired")
                .reason,
        }];
        for version in [3, CURRENT_SCHEMA_VERSION] {
            let raw =
                format!("schema_version = {version}\n{UNTOUCHED_V3_BODY}{RETIRED_DASHBOARD_TABLE}");

            let (migrated, notices) = migrate_file_with_notices(&raw)
                .unwrap()
                .unwrap_or_else(|| panic!("V{version}: the retired table must trigger a rewrite"));
            assert_eq!(notices, expected, "V{version}");
            assert_eq!(
                migrated,
                format!("schema_version = {CURRENT_SCHEMA_VERSION}\n{UNTOUCHED_V3_BODY}"),
                "V{version}: only the retired table and the version may change"
            );

            let load = migrate_to_current_salvaged(&raw);
            assert_eq!(load.notices, expected, "V{version} load");

            let (out, notices) = apply_doc(&raw);
            assert_eq!(notices, expected, "V{version} document cleanup");
            assert_eq!(
                out,
                raw.replace(RETIRED_DASHBOARD_TABLE, ""),
                "V{version} document cleanup"
            );
        }
    }

    /// Every other key the V2 -> V3 step drops, as found in a V3 or V4 file
    /// next to the live keys of the same sections, which must stay.
    const V3_RETIRED_BODY: &str = "locale = \"en\"\n\n\
         [swarms.research]\nstrategy = \"sequential\"\n\n\
         [reliability]\nprovider_retries = 3\nfallback_providers = [\"openai\"]\n\
         model_fallbacks = { fast = [\"gpt-4o-mini\"] }\n\n\
         [tts]\nenabled = true\ndefault_provider = \"openai\"\n\n\
         [transcription]\nenabled = true\ndefault_provider = \"groq\"\n\
         default_model_provider = \"groq\"\ndefault_transcription_provider = \"groq\"\n\n\
         [identity]\nformat = \"openclaw\"\n";

    const V3_RETIRED_PATHS: [&str; 8] = [
        "identity",
        "reliability.fallback_providers",
        "reliability.model_fallbacks",
        "swarms",
        "transcription.default_model_provider",
        "transcription.default_provider",
        "transcription.default_transcription_provider",
        "tts.default_provider",
    ];

    #[test]
    fn keys_dropped_by_the_v3_step_are_retired_on_every_path() {
        for version in [3, CURRENT_SCHEMA_VERSION] {
            let raw = format!("schema_version = {version}\n{V3_RETIRED_BODY}");

            let (migrated, notices) = migrate_file_with_notices(&raw)
                .unwrap()
                .unwrap_or_else(|| panic!("V{version}: retired keys must trigger a rewrite"));
            assert_eq!(removed_paths(&notices), V3_RETIRED_PATHS, "V{version}");
            let value: toml::Value = toml::from_str(&migrated).unwrap();
            assert_eq!(
                value["reliability"]["provider_retries"].as_integer(),
                Some(3)
            );
            assert_eq!(value["tts"]["enabled"].as_bool(), Some(true));
            assert_eq!(value["transcription"]["enabled"].as_bool(), Some(true));
            assert_eq!(value["locale"].as_str(), Some("en"));
            for gone in [
                "swarms",
                "fallback_providers",
                "model_fallbacks",
                "default_provider",
                "default_model_provider",
                "default_transcription_provider",
                "[identity]",
            ] {
                assert!(
                    !migrated.contains(gone),
                    "V{version} kept {gone}:\n{migrated}"
                );
            }

            let load = migrate_to_current_salvaged(&raw);
            assert_eq!(
                removed_paths(&load.notices),
                V3_RETIRED_PATHS,
                "V{version} load"
            );
            assert_eq!(load.config.reliability.provider_retries, 3);

            let (out, notices) = apply_doc(&raw);
            assert_eq!(
                removed_paths(&notices),
                V3_RETIRED_PATHS,
                "V{version} cleanup"
            );
            assert!(out.contains("provider_retries = 3"), "{out}");
        }
    }

    #[test]
    fn a_v2_config_still_lifts_its_identity_into_agents() {
        // The V3 entries apply only after the V2 -> V3 step, so that step
        // still moves a top-level `[identity]` into the agents rather than
        // the table deleting it first.
        let raw = "schema_version = 2\n\n[identity]\nformat = \"openclaw\"\n\n\
                   [agents.helper]\nprovider = \"openai\"\nmodel = \"gpt-4o-mini\"\n";
        let migrated = migrate_to_current(raw).expect("a V2 config migrates");
        assert!(
            migrated
                .agents
                .values()
                .any(|agent| agent.identity.format == "openclaw"),
            "the identity must land on the agents"
        );
        let (_, notices) = migrate_file_with_notices(raw).unwrap().unwrap();
        assert!(
            !notices
                .iter()
                .any(|n| matches!(n, MigrationNotice::Removed { path, .. } if path == "identity")),
            "a moved identity is not reported as removed: {notices:?}"
        );
    }
}
