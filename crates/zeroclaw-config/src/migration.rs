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
    /// A `ZEROCLAW_*` environment variable names a retired key, so it was
    /// ignored instead of failing startup.
    IgnoredEnvOverride {
        variable: String,
        reason: &'static str,
    },
}

impl MigrationNotice {
    /// Whether the notice reports a change to the config file, as opposed to
    /// an ignored environment variable, which leaves the file as it is. Only
    /// changing notices make a migration write the file.
    #[must_use]
    pub fn changes_file(&self) -> bool {
        !matches!(self, Self::IgnoredEnvOverride { .. })
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
            Self::IgnoredEnvOverride { variable, reason } => {
                format!("ignored `{variable}`, which sets a retired config key: {reason}")
            }
        }
    }
}

/// A [`RetiredKey::path`] segment that matches every key of the table at that
/// level, e.g. `&["agents", ANY_KEY, "max_tool_iterations"]` retires the key in
/// every `[agents.<alias>]` block.
pub const ANY_KEY: &str = "*";

/// One retired config key. The migration that reaches `retired_in` deletes
/// it, with everything under it.
#[derive(Debug, Clone, Copy)]
pub struct RetiredKey {
    /// The schema version whose migration step retires the key.
    pub retired_in: u32,
    /// Path from the config root, one segment per table key. [`ANY_KEY`]
    /// matches every key at its level. It covers every spelling the schema
    /// accepts for its keys too (see `ALIASED_KEYS`): `channels.wati` also
    /// retires `channels_config.wati`.
    pub path: &'static [&'static str],
    /// Why it was retired and what to use instead, in English: logs and
    /// `--json` output carry it as is.
    pub reason: &'static str,
    /// The Fluent id of [`Self::reason`] in the CLI catalogue
    /// (`cli-config-retired-reason-*`), which the CLI shows the operator in
    /// their locale. Entries that share a reason share its id.
    pub reason_id: &'static str,
}

/// Every retired key, applied by the migration chain when it reaches
/// `retired_in`, and again on every load of a current config so a key that
/// reappears is dropped with a notice rather than silently ignored. Retiring
/// another key is one entry here plus removing its schema field, its reason in
/// every locale's `cli.ftl` (unless it shares one), and a version bump (a new
/// `MIGRATION_STEPS` entry) if no pending version already covers it.
pub const RETIRED_KEYS: &[RetiredKey] = &[
    RetiredKey {
        retired_in: 4,
        path: &["security", "nevis"],
        reason: "the Nevis IAM integration was removed; configure `[oidc.<alias>]` with \
                 `[users]` and `[permission_profiles]` instead",
        reason_id: "cli-config-retired-reason-nevis",
    },
    // Agent-inline runtime tunables: superseded by runtime profiles and no
    // longer read from `[agents.<alias>]`, so serde ignored them silently.
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "compact_context"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "max_tool_iterations"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "max_history_messages"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "max_context_tokens"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "memory_recall_limit"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "parallel_tools"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "tool_dispatcher"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &["agents", ANY_KEY, "strict_tool_parsing"],
        reason: INERT_AGENT_TUNABLE,
        reason_id: "cli-config-retired-reason-agent-tunable",
    },
    RetiredKey {
        retired_in: 4,
        path: &[
            "runtime_profiles",
            ANY_KEY,
            "context_compression",
            "summary_model",
        ],
        reason: "the deprecated bare model id had no provider identity and was never read; \
                 `summary_provider` replaced it (context compression itself is not \
                 currently implemented in the runtime)",
        reason_id: "cli-config-retired-reason-summary-model",
    },
    // Retired before V4 and until now handled outside this table: the
    // migration chain stripped `[node_transport]` silently after every step
    // from V2 on, and load only warned about WATI sections. Under V4 they
    // follow the same policy, so every load, migration and incremental save
    // removes them, secrets included.
    RetiredKey {
        retired_in: 2,
        path: &["node_transport"],
        reason: "the legacy HMAC node transport was removed",
        reason_id: "cli-config-retired-reason-node-transport",
    },
    RetiredKey {
        retired_in: 4,
        path: &["channels", "wati"],
        reason: RETIRED_WATI,
        reason_id: "cli-config-retired-reason-wati",
    },
    // Spellings no schema field reads, so serde would ignore them silently.
    // Twitter and Reddit stay channels (`[channels.twitter.<alias>]`,
    // `[channels.reddit.<alias>]`) and Notion stays the top-level `[notion]`
    // section; only these other spellings are dropped. The `[notion]` poller
    // is registered as the `notion` channel, so references to `notion` in
    // agent channels, escalation and peer groups are live and are left alone.
    RetiredKey {
        retired_in: 4,
        path: &["twitter"],
        reason: "a top-level `[twitter]` section is not read; configure the Twitter channel as \
                 `[channels.twitter.<alias>]`",
        reason_id: "cli-config-retired-reason-twitter",
    },
    RetiredKey {
        retired_in: 4,
        path: &["reddit"],
        reason: "a top-level `[reddit]` section is not read; configure the Reddit channel as \
                 `[channels.reddit.<alias>]`",
        reason_id: "cli-config-retired-reason-reddit",
    },
    RetiredKey {
        retired_in: 4,
        path: &["channels", "notion"],
        reason: "a `[channels.notion]` table is not read; Notion is configured in the top-level \
                 `[notion]` section, whose database poller is the `notion` channel",
        reason_id: "cli-config-retired-reason-notion",
    },
    // Retired by the V2 -> V3 step, whose normalizer also surfaces the shared
    // `[gateway.pairing_code]` policy. This entry covers files already at V3
    // or later, which never pass through that step.
    RetiredKey {
        retired_in: 3,
        path: &["gateway", "pairing_dashboard", "code_length"],
        reason: "the dashboard no longer has its own pairing-code length; \
                 `[gateway.pairing_code]` `length` sets it for every pairing surface",
        reason_id: "cli-config-retired-reason-pairing-code-length",
    },
    // Also dropped by the V2 -> V3 step, which moves or discards them while
    // restructuring. No current field reads them, so a V3 or later file that
    // still holds one would otherwise keep it forever.
    RetiredKey {
        retired_in: 3,
        path: &["swarms"],
        reason: "swarms were removed in V3",
        reason_id: "cli-config-retired-reason-swarms",
    },
    RetiredKey {
        retired_in: 3,
        path: &["reliability", "fallback_providers"],
        reason: RETIRED_GLOBAL_FALLBACK,
        reason_id: "cli-config-retired-reason-global-fallback",
    },
    RetiredKey {
        retired_in: 3,
        path: &["reliability", "model_fallbacks"],
        reason: RETIRED_GLOBAL_FALLBACK,
        reason_id: "cli-config-retired-reason-global-fallback",
    },
    RetiredKey {
        retired_in: 3,
        path: &["tts", "default_provider"],
        reason: "there is no global default TTS provider; set `tts_provider` on each \
                 `[agents.<alias>]` instead",
        reason_id: "cli-config-retired-reason-tts-default",
    },
    RetiredKey {
        retired_in: 3,
        path: &["transcription", "default_provider"],
        reason: RETIRED_GLOBAL_TRANSCRIPTION,
        reason_id: "cli-config-retired-reason-global-transcription",
    },
    RetiredKey {
        retired_in: 3,
        path: &["transcription", "default_model_provider"],
        reason: RETIRED_GLOBAL_TRANSCRIPTION,
        reason_id: "cli-config-retired-reason-global-transcription",
    },
    RetiredKey {
        retired_in: 3,
        path: &["transcription", "default_transcription_provider"],
        reason: RETIRED_GLOBAL_TRANSCRIPTION,
        reason_id: "cli-config-retired-reason-global-transcription",
    },
    RetiredKey {
        retired_in: 3,
        path: &["identity"],
        reason: "identity is set per agent; move it to `[agents.<alias>.identity]`",
        reason_id: "cli-config-retired-reason-identity",
    },
];

/// The retired key a config path lies at or under, matching [`ANY_KEY`]
/// segments against any key. Used where a path arrives from outside the
/// file, such as an environment override, to recognize a retired key rather
/// than fail on it as unknown.
#[must_use]
pub fn retired_key_covering(path: &[&str]) -> Option<&'static RetiredKey> {
    RETIRED_KEYS.iter().find(|key| {
        spellings_of(key.path).iter().any(|pattern| {
            pattern.len() <= path.len()
                && pattern
                    .iter()
                    .zip(path)
                    .all(|(pattern, segment)| *pattern == ANY_KEY || pattern == segment)
        })
    })
}

/// The Fluent id of a retirement reason, for showing it in the operator's
/// locale. Notices carry the English reason (see [`RetiredKey::reason`]);
/// `None` for text that is not a [`RETIRED_KEYS`] reason.
#[must_use]
pub fn retirement_reason_id(reason: &str) -> Option<&'static str> {
    RETIRED_KEYS
        .iter()
        .find(|key| key.reason == reason)
        .map(|key| key.reason_id)
}

const RETIRED_GLOBAL_FALLBACK: &str = "the global fallback lists were removed in V3; set \
     `fallback` on each `[providers.models.<type>.<alias>]` instead";

const RETIRED_GLOBAL_TRANSCRIPTION: &str = "there is no global default transcription provider; \
     set `transcription_provider` on each `[agents.<alias>]` instead";

const RETIRED_WATI: &str = "WATI support was removed; migrate to `[channels.whatsapp.<alias>]` \
     using the Cloud API or WhatsApp Web, then revoke the unused WATI API token";

const INERT_AGENT_TUNABLE: &str = "agent-inline runtime tunables are no longer read; runtime \
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
/// layout is plainly V3: it holds a section only the V3 layout has, and
/// nothing only an older layout has (see `unversioned_shape`).
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
            UnversionedShape::Mixed {
                current,
                legacy,
                retired,
            } => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "current": current, "legacy": legacy })),
                    "config has no schema_version and mixes the V3 layout with an older one"
                );
                let fix = match retired {
                    Some((path, reason)) => format!(
                        "`{path}` is retired ({reason}), so if the rest of the file is current, \
                         adding `schema_version = 4` as its first line is the whole fix: the \
                         current layout then drops it with a notice. If the whole file is \
                         older, add `schema_version = 1` or `schema_version = 2` instead"
                    ),
                    None => "Rewrite the older settings in the current layout and add \
                             `schema_version = 4` as the first line, or, if the whole file is \
                             older, add `schema_version = 1` or `schema_version = 2` instead; \
                             `zeroclaw config migrate` then rewrites it in the current layout"
                        .to_string(),
                };
                anyhow::bail!(
                    "config has no `schema_version` and mixes layouts: {current}, but {legacy}. \
                     Read either way, part of it would be lost, so it is not guessed. {fix}"
                )
            }
            UnversionedShape::Ambiguous { detail } => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "detail": detail })),
                    "config has no schema_version and its shape is ambiguous"
                );
                anyhow::bail!(
                    "config has no `schema_version`, and {detail}. Add `schema_version` as the \
                     first line of the file with the version it was written for, so it is not \
                     guessed: `schema_version = 1` or `schema_version = 2` for an older file, \
                     `schema_version = 3` or `schema_version = 4` for the current layout"
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

/// How a config with no `schema_version` key reads, by its layout.
enum UnversionedShape {
    /// The V3 layout and nothing older: read it as [`INFERRED_SCHEMA_VERSION`].
    V3,
    /// Nothing only the V3 layout has: keep the historical V1 reading.
    NotV3,
    /// A section only the V3 layout has beside a setting only an older one
    /// has. Either reading reshapes or drops part of the file, so neither is
    /// chosen. `retired` names the older setting, and the reason, when the
    /// current layout retires it anyway, so stating the current version is
    /// the whole fix.
    Mixed {
        current: String,
        legacy: String,
        retired: Option<(String, &'static str)>,
    },
    /// A child table of an alias-keyed section reads both as a field of a V2
    /// entry and as a V3 alias, so the operator must state the version.
    /// `detail` says which.
    Ambiguous { detail: String },
}

/// The V2 `[cron]` subsystem knobs, which V3 moved to `[scheduler]`.
const V2_CRON_KNOBS: &[&str] = &["enabled", "catch_up_on_startup", "max_run_history"];

/// Settings only the layouts before V3 have: the V1 -> V2 -> V3 chain reads,
/// moves, renames or drops each, and the current schema does not read it
/// where it is. A file holding one was written in an older layout.
/// [`legacy_layout_evidence`] adds the shapes a path cannot express. Pinned
/// both ways: `every_legacy_layout_example_is_read_as_v1_and_refused_beside_v3`
/// shows the chain reads each and the current schema does not, and
/// `current_sections_never_read_as_an_older_layout` that no current section
/// holds one.
const LEGACY_LAYOUT_PATHS: &[&[&str]] = &[
    // V1's top-level provider settings (`V1Config`) and its spellings of
    // them, which V2 folded under `[providers]`.
    &["api_key"],
    &["api_url"],
    &["api_path"],
    &["default_provider"],
    &["model_provider"],
    &["default_model"],
    &["model"],
    &["model_providers"],
    &["default_temperature"],
    &["provider_timeout_secs"],
    &["provider_max_tokens"],
    &["extra_headers"],
    // Sections V3 split into profiles, lifted into agents, or dropped.
    &["autonomy"],
    &["agent"],
    &["swarms"],
    &["identity"],
    &["node_transport"],
    // Settings the V2 -> V3 step moved, renamed or dropped.
    &["providers", "extra_headers"],
    &["security", "sandbox"],
    &["security", "resources"],
    &["reliability", "fallback_providers"],
    &["reliability", "model_fallbacks"],
    &["memory", "qdrant"],
    &["memory", "postgres"],
    &["memory", "sqlite_open_timeout_secs"],
    &["storage", "provider"],
    &["cost", "prices"],
    &["tts", "openai"],
    &["tts", "elevenlabs"],
    &["tts", "google"],
    &["tts", "edge"],
    &["tts", "piper"],
    &["tts", "default_provider"],
    &["transcription", "default_provider"],
    &["transcription", "default_model_provider"],
    &["transcription", "default_transcription_provider"],
    &["gateway", "pairing_dashboard", "code_length"],
    &["tunnel", "provider"],
    &["web_search", "provider"],
    &["channels", "discord_history"],
    &["channels", "feishu"],
];

/// How a config with no `schema_version` key is to be read.
///
/// A missing key historically meant V1, since V1 files predate the key. But a
/// hand-written or template-generated V3 file can omit it too, and running
/// such a file through the V1 migration silently destroys it: alias-keyed
/// channel sections collapse into `default`, and declarative cron jobs are
/// dropped. So the file's layout is weighed both ways, each by a definition
/// rather than a guess.
///
/// Evidence of an older layout is a setting only the layouts before V3 have
/// (see [`legacy_layout_evidence`]), or a provider or channel section holding
/// fields directly (`[providers.models.ollama] model = "..."`), the V2 shape.
///
/// Evidence of the V3 layout is a setting only it has, which the V1 reading
/// would reshape, or merge an older setting into:
/// - an alias-keyed section, `providers.models.<family>` or `channels.<type>`
///   (also under `channels_config`, a spelling the current schema accepts),
///   meaning a table whose every value is a table
///   (`[providers.models.ollama.default]`, `[channels.discord.work]`);
/// - a child table of a section holding V2 fields that is not one of those
///   fields with what it actually holds (see [`holds_a_v2_field`]), so a V3
///   alias beside them;
/// - a declarative cron job, a table under `[cron]` (`[cron.nightly]`);
/// - a V3-only setting the chain also writes (see [`v3_only_destination`]):
///   TTS and transcription provider aliases, the profiles and peer groups,
///   alias-keyed storage, `tunnel_provider`, `search_provider`, and the keys
///   of a current agent.
///
/// A file with both is refused as mixed: either reading reshapes or drops
/// part of it. A file with V3 evidence alone is read as V3, unless a child of
/// an alias-keyed section also reads as a field of a V2 entry (see
/// [`keys_read_as_v2_entry_fields`]): `[providers.models.openai.
/// extra_headers]` is either the V2 `openai` profile's headers or a V3 alias
/// named `extra_headers`, so the version is required. Any other file keeps
/// the V1 reading. With no older setting in it, that reading configures what
/// the V3 reading would (pinned by
/// `a_file_with_no_layout_evidence_reads_the_same_as_v1_and_as_v3`), with one
/// deliberate difference: an agent holding no current agent setting, such as
/// one with only `system_prompt` or `max_tool_iterations`, is read as the V2
/// agent it can only be, as before V4. The V1 reading binds it to profiles
/// and moves its `max_tool_iterations` to its runtime profile, where V2 read
/// it; the V3 reading would leave an agent with no model provider and retire
/// the limit (pinned by `keys_retired_at_v4_read_the_same_or_as_a_v2_agent`).
/// An empty provider family or channel type is no evidence either way: it
/// holds nothing to lose.
///
/// A V4 file has the V3 shape, so an unversioned V4 file is read as V3 too;
/// its V3 -> V4 step changes nothing it has not already done.
fn unversioned_shape(table: &toml::Table) -> UnversionedShape {
    let provider_families = table
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get("models"))
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|families| {
            families.iter().map(|(family, section)| {
                (
                    V2EntryKind::ModelProvider,
                    "providers.models",
                    family.as_str(),
                    section,
                )
            })
        });
    // `channels_config`, V1's name for the section, is also a spelling the
    // current schema accepts, so both are read by their shape.
    let channel_types = spellings_of(&["channels"])
        .into_iter()
        .map(|spelling| spelling[0])
        .filter_map(|root| {
            table
                .get(root)
                .and_then(toml::Value::as_table)
                .map(|channels| (root, channels))
        })
        .flat_map(|(root, channels)| {
            crate::schema::v2::V3_CHANNEL_TYPES
                .iter()
                .filter_map(move |kind| {
                    channels
                        .get(*kind)
                        .map(|section| (V2EntryKind::Channel, root, *kind, section))
                })
        });

    let mut current = None;
    let mut legacy = legacy_layout_evidence(table);
    let mut ambiguous = None;
    for (entry_kind, root, name, section) in provider_families.chain(channel_types) {
        let Some(section) = section.as_table() else {
            continue;
        };
        if section.is_empty() {
            continue;
        }
        let path = format!("{root}.{name}");
        if !section.values().all(toml::Value::is_table) {
            // A field held directly on the section: the V2 shape.
            legacy.get_or_insert_with(|| {
                (
                    format!("`[{path}]` holds fields directly, the V2 layout"),
                    None,
                )
            });
            // A child table that is not a field of the V2 entry, with what it
            // holds, is a V3 alias beside those fields: both layouts in one
            // section.
            if let Some((alias, _)) = section.iter().find(|(key, value)| {
                value.is_table() && !holds_a_v2_field(entry_kind, name, section, key)
            }) {
                current.get_or_insert_with(|| format!("`[{path}.{alias}]` is a V3 alias"));
            }
            continue;
        }
        current.get_or_insert_with(|| format!("`[{path}]` is in the V3 layout"));
        if ambiguous.is_none()
            && let Some(key) = keys_read_as_v2_entry_fields(entry_kind, name, section)
                .into_iter()
                .next()
        {
            ambiguous = Some(format!(
                "`[{path}.{key}]` reads either as the `{key}` field of a V2 `{path}` \
                 entry or as a V3 alias named `{key}`"
            ));
        }
    }
    if let Some(cron) = table.get("cron").and_then(toml::Value::as_table)
        && let Some((name, _)) = cron.iter().find(|(_, value)| value.is_table())
    {
        current.get_or_insert_with(|| format!("`[cron.{name}]` is a V3 cron job"));
    }
    if current.is_none() {
        current = v3_only_destination(table);
    }

    match (current, legacy) {
        (Some(current), Some((legacy, retired))) => UnversionedShape::Mixed {
            current,
            legacy,
            retired,
        },
        (Some(_), None) => match ambiguous {
            Some(detail) => UnversionedShape::Ambiguous { detail },
            None => UnversionedShape::V3,
        },
        (None, _) => UnversionedShape::NotV3,
    }
}

/// Keys the current schema reads under a second spelling (its serde
/// aliases), as `(parent, alias, written)`: `written` is the spelling the
/// schema writes, and [`ANY_KEY`] in `parent` matches any key. An incremental
/// save writes a setting under its written spelling, so a file spelling it
/// the other way would end up holding both, which the schema refuses to read
/// (`duplicate field`); saves rewrite those keys first (see
/// [`use_written_spellings`]), and the detector reads older-layout settings
/// under every spelling (see [`spellings_of`]). Aliases of enum values
/// (`otlp`), `V1Config`'s spellings and types that are not config are not
/// keys of the file. Kept in step with the types by
/// `every_serde_alias_is_listed_with_its_field`.
pub(crate) const ALIASED_KEYS: &[(&[&str], &str, &str)] = &[
    (&[], "channels_config", "channels"),
    (&[], "mcpServers", "mcp"),
    (&["mcp"], "mcpServers", "servers"),
    (
        &["providers", "models", "azure", ANY_KEY],
        "azure_openai_resource",
        "resource",
    ),
    (
        &["providers", "models", "azure", ANY_KEY],
        "azure_openai_deployment",
        "deployment",
    ),
    (
        &["providers", "models", "azure", ANY_KEY],
        "azure_openai_api_version",
        "api_version",
    ),
    (&["providers", "tts", "openai", ANY_KEY], "api_url", "uri"),
    (
        &["providers", "tts", "elevenlabs", ANY_KEY],
        "api_url",
        "uri",
    ),
    (&["providers", "tts", "google", ANY_KEY], "api_url", "uri"),
    (&["providers", "tts", "edge", ANY_KEY], "api_url", "uri"),
    (&["providers", "tts", "piper", ANY_KEY], "api_url", "uri"),
    (&["skills"], "install-suggestions", "install_suggestions"),
    (&["composio"], "enable", "enabled"),
    (&["microsoft365"], "enable", "enabled"),
    (&["storage", "postgres", ANY_KEY], "dbURL", "db_url"),
    (&["storage", "postgres", ANY_KEY], "database_url", "db_url"),
    (&["storage", "postgres", ANY_KEY], "databaseUrl", "db_url"),
    (&["observability"], "runtime_trace_mode", "log_persistence"),
    (
        &["observability"],
        "runtime_trace_path",
        "log_persistence_path",
    ),
    (
        &["observability"],
        "runtime_trace_max_entries",
        "log_persistence_max_entries",
    ),
    (&["risk_profiles", ANY_KEY], "allowed_path", "allowed_roots"),
    (
        &["risk_profiles", ANY_KEY],
        "allowed_paths",
        "allowed_roots",
    ),
    (&["heartbeat"], "channel", "target"),
    (&["heartbeat"], "recipient", "to"),
];

/// `pattern` under every spelling the schema accepts for its keys (see
/// [`ALIASED_KEYS`]): `channels.feishu` is also `channels_config.feishu`.
fn spellings_of(pattern: &[&'static str]) -> Vec<Vec<&'static str>> {
    let mut spellings: Vec<Vec<&'static str>> = vec![Vec::new()];
    for (at, segment) in pattern.iter().enumerate() {
        let parent = &pattern[..at];
        let mut longer = Vec::new();
        for prefix in &spellings {
            longer.push([prefix.as_slice(), &[*segment]].concat());
            for (alias_parent, alias, written) in ALIASED_KEYS {
                if written == segment && *alias_parent == parent {
                    longer.push([prefix.as_slice(), &[*alias]].concat());
                }
            }
        }
        spellings = longer;
    }
    spellings
}

/// Rewrite every key of `root` spelled as one of [`ALIASED_KEYS`]' aliases to
/// the spelling the schema writes, keeping its value, comments and position,
/// so an incremental edit of that setting lands on it rather than beside it.
/// A table holding two spellings of one setting, its written spelling beside
/// an alias or two of its aliases, is refused: the schema cannot read it, so
/// no save may write over it.
pub(crate) fn use_written_spellings(root: &mut toml_edit::Table) -> Result<()> {
    // Outer sections first, so a nested alias is found under the section's
    // written spelling whatever order `ALIASED_KEYS` lists them in.
    let mut aliased: Vec<&(&[&str], &str, &str)> = ALIASED_KEYS.iter().collect();
    aliased.sort_by_key(|(parent, _, _)| parent.len());
    for (parent, alias, written) in aliased {
        let spellings: Vec<&str> = std::iter::once(*written)
            .chain(
                ALIASED_KEYS
                    .iter()
                    .filter(|(other_parent, _, other_written)| {
                        other_parent == parent && other_written == written
                    })
                    .map(|(_, other, _)| *other),
            )
            .collect();
        let parents = if parent.is_empty() {
            vec![Vec::new()]
        } else {
            expand_doc_path(root, parent)
        };
        for path in parents {
            let table: &mut dyn RenameableTable = if path.is_empty() {
                &mut *root
            } else {
                match item_at(root, &path) {
                    Some(toml_edit::Item::Table(table)) => table,
                    Some(toml_edit::Item::Value(toml_edit::Value::InlineTable(table))) => table,
                    _ => continue,
                }
            };
            let held: Vec<String> = spellings
                .iter()
                .filter(|spelling| table.has(spelling))
                .map(|spelling| format!("`{spelling}`"))
                .collect();
            if let [first @ .., last] = held.as_slice()
                && !first.is_empty()
            {
                let at = if path.is_empty() {
                    "the top level".to_string()
                } else {
                    format!("`[{}]`", path.join("."))
                };
                anyhow::bail!(
                    "{at} holds {} and {last}, spellings of one setting that the schema \
                     cannot read together; keep one before saving",
                    first.join(", ")
                );
            }
            rename_key_in_place(table, alias, written);
        }
    }
    Ok(())
}

/// Refuse an older file that still holds two spellings of one setting once
/// its structural migration has run, by the check [`use_written_spellings`]
/// makes of a current one. The migration chooses between some spellings
/// itself (a V1 file's `channels_config` over its `channels`); a conflict it
/// leaves, such as a heartbeat's `channel` beside its `target` or a Postgres
/// entry's `dbURL` beside its `database_url`, makes the file unreadable and
/// the config loaded from it lack that whole section, so a save that wrote
/// the loaded config over it would drop the section. Only checks: the chain
/// runs on a copy, and nothing is written.
pub(crate) fn refuse_spellings_left_by_migration(existing: &str) -> Result<()> {
    let value: toml::Value = toml::from_str(existing).context("failed to parse config TOML")?;
    let from = detect_version(&value)?;
    if from >= CURRENT_SCHEMA_VERSION {
        return Ok(());
    }
    let migrated = run_chain(value, from, &mut Vec::new())?;
    let mut migrated: toml_edit::DocumentMut = toml::to_string(&migrated)
        .context("failed to render the migrated config")?
        .parse()
        .context("failed to parse the migrated config")?;
    if let Err(err) = use_written_spellings(migrated.as_table_mut()) {
        anyhow::bail!("{err} (the file is V{from}, and migrating it does not choose between them)");
    }
    Ok(())
}

/// The item at `path` under `root`, or `None`.
fn item_at<'a>(root: &'a mut toml_edit::Table, path: &[String]) -> Option<&'a mut toml_edit::Item> {
    let (first, rest) = path.split_first()?;
    let mut item = root.get_mut(first)?;
    for key in rest {
        item = item.as_table_like_mut()?.get_mut(key)?;
    }
    Some(item)
}

/// A table or inline table whose keys [`rename_key_in_place`] can rename.
trait RenameableTable {
    fn entry_keys(&self) -> Vec<String>;
    fn has(&self, key: &str) -> bool;
    /// Rebuild the table in its own order, `rename` deciding each key.
    fn rebuild(&mut self, rename: &dyn Fn(toml_edit::Key) -> toml_edit::Key);
}

impl RenameableTable for toml_edit::Table {
    fn entry_keys(&self) -> Vec<String> {
        self.iter().map(|(key, _)| key.to_string()).collect()
    }
    fn has(&self, key: &str) -> bool {
        self.contains_key(key)
    }
    fn rebuild(&mut self, rename: &dyn Fn(toml_edit::Key) -> toml_edit::Key) {
        let keys = self.entry_keys();
        let entries: Vec<(toml_edit::Key, toml_edit::Item)> = keys
            .iter()
            .filter_map(|key| self.remove_entry(key))
            .collect();
        for (key, item) in entries {
            self.insert_formatted(&rename(key), item);
        }
    }
}

impl RenameableTable for toml_edit::InlineTable {
    fn entry_keys(&self) -> Vec<String> {
        self.iter().map(|(key, _)| key.to_string()).collect()
    }
    fn has(&self, key: &str) -> bool {
        self.contains_key(key)
    }
    fn rebuild(&mut self, rename: &dyn Fn(toml_edit::Key) -> toml_edit::Key) {
        let keys = self.entry_keys();
        let entries: Vec<(toml_edit::Key, toml_edit::Value)> = keys
            .iter()
            .filter_map(|key| self.remove_entry(key))
            .collect();
        for (key, value) in entries {
            self.insert_formatted(&rename(key), value);
        }
    }
}

/// Rename `alias` in `table` to `written`, keeping the entry's place among
/// its siblings and the comments around its key and value. Nothing is
/// renamed when the table already holds `written`.
fn rename_key_in_place(table: &mut dyn RenameableTable, alias: &str, written: &str) {
    if !table.has(alias) || table.has(written) {
        return;
    }
    table.rebuild(&|key| {
        if key.get() != alias {
            return key;
        }
        let mut renamed = toml_edit::Key::new(written);
        *renamed.leaf_decor_mut() = key.leaf_decor().clone();
        *renamed.dotted_decor_mut() = key.dotted_decor().clone();
        renamed
    });
}

/// Settings only the V3 layout has that the V1 -> V3 chain also writes into:
/// beside the older setting it comes from, a migration would merge the two
/// and keep only one, so each is evidence of the V3 layout. None of them
/// existed in V2: the last V2 `Config`, before the V3 multi-agent schema,
/// had none of these sections or keys. The V2 sections the chain also writes into
/// (`[scheduler]`) are no evidence. [`ANY_KEY`] matches any key; each must
/// hold a value. Pinned by
/// `a_legacy_setting_beside_what_its_migration_writes_is_refused`.
const V3_ONLY_DESTINATIONS: &[&[&str]] = &[
    &["providers", "tts", ANY_KEY],
    &["providers", "transcription", ANY_KEY],
    &["risk_profiles", ANY_KEY],
    &["runtime_profiles", ANY_KEY],
    &["peer_groups", ANY_KEY],
    &["storage", "sqlite"],
    &["storage", "postgres"],
    &["storage", "qdrant"],
    &["storage", "markdown"],
    &["storage", "lucid"],
    &["tunnel", "tunnel_provider"],
    &["web_search", "search_provider"],
];

/// The keys of a current `[agents.<alias>]`. V2's agents had none of them,
/// and the chain writes several (`model_provider`, `identity`), so an agent
/// holding one is evidence of the V3 layout. Kept in step with the schema by
/// `v3_agent_fields_are_the_current_agent_fields`.
const V3_AGENT_FIELDS: &[&str] = &[
    "a2a",
    "acp_enable_mcp",
    "channels",
    "classifier_provider",
    "cron_jobs",
    "delegate_same_risk_profile",
    "delegates",
    "enabled",
    "identity",
    "knowledge_bundles",
    "mcp_bundles",
    "memory",
    "model_provider",
    "precheck",
    "risk_profile",
    "runtime_profile",
    "skill_bundles",
    "summary_provider",
    "transcription_provider",
    "tts_provider",
    "workspace",
];

/// The first V3-only setting in `table` the chain also writes (see
/// [`V3_ONLY_DESTINATIONS`] and [`V3_AGENT_FIELDS`]), described, or `None`.
fn v3_only_destination(table: &toml::Table) -> Option<String> {
    if let Some(path) = V3_ONLY_DESTINATIONS
        .iter()
        .flat_map(|pattern| spellings_of(pattern))
        .flat_map(|pattern| expand_path(table, &pattern))
        .find(|path| value_at_path(table, path).is_some_and(holds_a_value))
    {
        return Some(format!("`{}` is a V3 setting", path.join(".")));
    }
    let agents = table.get("agents").and_then(toml::Value::as_table)?;
    agents.iter().find_map(|(alias, agent)| {
        let agent = agent.as_table()?;
        let key = V3_AGENT_FIELDS
            .iter()
            .find(|key| agent.get(**key).is_some_and(holds_a_value))?;
        Some(format!("`agents.{alias}.{key}` is a V3 setting"))
    })
}

/// The value at `path` under `table`, or `None`.
fn value_at_path<'a>(table: &'a toml::Table, path: &[String]) -> Option<&'a toml::Value> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(table.get(first.as_str())?, |value, key| {
            value.get(key.as_str())
        })
}

/// Whether `value` holds anything: a scalar, a non-empty array, or a table
/// with such a value somewhere beneath.
fn holds_a_value(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => table.values().any(holds_a_value),
        toml::Value::Array(items) => !items.is_empty(),
        _ => true,
    }
}

/// The first setting in `table` that only a layout before V3 has, described
/// with its path when the current layout retires it, or `None`: a path in [`LEGACY_LAYOUT_PATHS`], the V2 cron layout
/// (`[[cron.jobs]]`, or a subsystem knob on `[cron]`), a route naming its
/// provider `provider`, or a V2-only setting (see [`v2_only_marker`]). A
/// provider or channel section holding fields directly is found by
/// [`unversioned_shape`], which reads those sections.
fn legacy_layout_evidence(table: &toml::Table) -> Option<(String, Option<(String, &'static str)>)> {
    if let Some(path) = LEGACY_LAYOUT_PATHS
        .iter()
        .flat_map(|pattern| spellings_of(pattern))
        .find_map(|pattern| expand_path(table, &pattern).into_iter().next())
    {
        let segments: Vec<&str> = path.iter().map(String::as_str).collect();
        let dotted = path.join(".");
        let retired = retired_key_covering(&segments).map(|key| (dotted.clone(), key.reason));
        return Some((format!("`{dotted}` is a pre-V3 setting"), retired));
    }
    if let Some(cron) = table.get("cron").and_then(toml::Value::as_table)
        && let Some((key, _)) = cron.iter().find(|(key, value)| {
            (key.as_str() == "jobs" && value.is_array())
                || (V2_CRON_KNOBS.contains(&key.as_str()) && !value.is_table())
        })
    {
        return Some((format!("`cron.{key}` is the V2 cron layout"), None));
    }
    if let Some(routes) = ["model_routes", "embedding_routes"]
        .into_iter()
        .find(|routes| {
            table
                .get(*routes)
                .and_then(toml::Value::as_array)
                .is_some_and(|entries| entries.iter().any(|entry| entry.get("provider").is_some()))
        })
    {
        return Some((
            format!(
                "`{routes}` entries name their provider `provider`, the V2 spelling of \
                 `model_provider`"
            ),
            None,
        ));
    }
    v2_only_marker(table).map(|detail| (detail, None))
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

/// A setting only V2 reads: a value held directly on `[providers]` (V3 keeps
/// only the `models`, `tts` and `transcription` tables there) or a V2-only
/// agent key. Read as V3 it would be ignored. `None` when there is no such
/// key.
fn v2_only_marker(table: &toml::Table) -> Option<String> {
    if let Some(providers) = table.get("providers").and_then(toml::Value::as_table)
        && let Some((key, _)) = providers.iter().find(|(_, value)| !value.is_table())
    {
        return Some(format!(
            "`providers.{key}` is a V2 setting the V3 layout does not read"
        ));
    }
    let agents = table.get("agents").and_then(toml::Value::as_table)?;
    agents.iter().find_map(|(alias, agent)| {
        let agent = agent.as_table()?;
        let key = V2_ONLY_AGENT_KEYS
            .iter()
            .find(|key| agent.contains_key(**key))?;
        Some(format!(
            "`agents.{alias}.{key}` is a V2 setting the V3 layout does not read"
        ))
    })
}

/// The kind of entry an alias-keyed section holds.
#[derive(Clone, Copy)]
enum V2EntryKind {
    ModelProvider,
    Channel,
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

/// Whether the child table `key` of a section holding V2 fields is one of
/// those fields with what it actually holds. A child named like no field of
/// the entry is not, and neither is one named like a field it cannot be: a
/// float in the `extra_headers` string map, a string in the `pricing` number
/// map. Either is a V3 alias beside the V2 fields. The name alone, which
/// [`keys_read_as_v2_entry_fields`] answers, decides only for an empty child.
fn holds_a_v2_field(kind: V2EntryKind, name: &str, section: &toml::Table, key: &str) -> bool {
    let Some(child) = section.get(key).and_then(toml::Value::as_table) else {
        return false;
    };
    if matches!(kind, V2EntryKind::ModelProvider) {
        // A family the current schema has no slot for, even under the name
        // the migration files it by, cannot be probed, and is no evidence
        // that its child is an alias.
        let (family, _) = crate::schema::v2::v2_profile_as_v3_entry(name, &toml::Table::new());
        if !crate::providers::ModelProviders::slot_names().contains(&family.as_str()) {
            return true;
        }
    }
    if child.is_empty() {
        return keys_read_as_v2_entry_fields(kind, name, section)
            .iter()
            .any(|field| field == key);
    }
    let mut probe: toml::Table = section
        .iter()
        .filter(|(_, value)| !value.is_table())
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect();
    probe.insert(key.to_string(), toml::Value::Table(child.clone()));
    round_trip_as_v2_entry(kind, name, &probe)
        .and_then(|entry| entry.get(key).cloned())
        .is_some_and(|kept| keeps_every_value(&toml::Value::Table(child.clone()), &kept))
}

/// Whether `kept` holds every value of `original` at the same path. A number
/// matches its value as either type, since a float field reads `1` as `1.0`.
fn keeps_every_value(original: &toml::Value, kept: &toml::Value) -> bool {
    match (original, kept) {
        (toml::Value::Table(original), toml::Value::Table(kept)) => {
            original.iter().all(|(key, value)| {
                kept.get(key)
                    .is_some_and(|kept| keeps_every_value(value, kept))
            })
        }
        (toml::Value::Array(original), toml::Value::Array(kept)) => {
            original.len() == kept.len()
                && original
                    .iter()
                    .zip(kept)
                    .all(|(original, kept)| keeps_every_value(original, kept))
        }
        (toml::Value::Integer(original), toml::Value::Float(_)) => {
            kept.as_float() == Some(*original as f64)
        }
        (original, kept) => original == kept,
    }
}

/// `section` deserialized as one entry of its real type and serialized back,
/// or `None` when it does not read as a single entry at all. A V2 profile is
/// first rewritten as the V2 -> V3 step rewrites it, under the family the
/// step files it by (`grok` as `xai`, a `custom:` URL as `custom`).
fn round_trip_as_v2_entry(
    kind: V2EntryKind,
    name: &str,
    section: &toml::Table,
) -> Option<toml::Table> {
    const PROBE: &str = "__unversioned_shape_probe__";
    let (name, section) = match kind {
        V2EntryKind::ModelProvider => crate::schema::v2::v2_profile_as_v3_entry(name, section),
        V2EntryKind::Channel => (name.to_string(), section.clone()),
    };
    let mut slot = toml::Table::new();
    slot.insert(PROBE.to_string(), toml::Value::Table(section));
    let mut wrapped = toml::Table::new();
    wrapped.insert(name.clone(), toml::Value::Table(slot));
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
        .and_then(|value| value.get(&name))
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
    // Legacy typed schemas may discard the `[tools]` selector. Carry the exact
    // raw value across their chain, including malformed values: strict
    // deserialize must reject those, and salvage drops them like any other
    // invalid section.
    let mut value = value;
    let original_tools = value
        .as_table_mut()
        .context("config root must be a TOML table")?
        .remove("tools");
    let selector_omitted = original_tools.is_none();
    let (mut value, notices) = if from == CURRENT_SCHEMA_VERSION {
        // A retired key can reappear in a current file (hand-edited, or
        // copied from an old example). The schema no longer has a field for
        // it, so serde would silently ignore it; drop it and say so instead.
        let mut notices = Vec::new();
        for version in 2..=CURRENT_SCHEMA_VERSION {
            apply_retired_keys(&mut value, version, RETIRED_KEYS, &mut notices);
        }
        if notices.is_empty() && !selector_omitted {
            return Ok(None);
        }
        (value, notices)
    } else {
        let mut notices: Vec<MigrationNotice> =
            unversioned_notice(&value, from).into_iter().collect();
        let value = run_chain(value, from, &mut notices)?;
        (value, notices)
    };
    value
        .as_table_mut()
        .context("migrated config is not a TOML table")?
        .insert(
            "tools".to_string(),
            original_tools.unwrap_or_else(omitted_tool_selection),
        );
    log_notices(&notices);
    Ok(Some(Migrated { value, notices }))
}

/// The `[tools]` selector a config that omits it is read with: the full
/// default selection. It is the only persisted compatibility state, so every
/// migration route writes it.
fn omitted_tool_selection() -> toml::Value {
    toml::Value::Table(toml::Table::from_iter([(
        "optional".to_string(),
        toml::Value::Array(vec![toml::Value::String("*".to_string())]),
    )]))
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
        let selector_omitted = !value
            .as_table()
            .is_some_and(|table| table.contains_key("tools"));
        let mut notices: Vec<MigrationNotice> =
            unversioned_notice(&value, from).into_iter().collect();
        notices.extend(apply_retired_keys_to_doc(doc.as_table_mut()));
        if from == CURRENT_SCHEMA_VERSION
            && !selector_omitted
            && !notices.iter().any(MigrationNotice::changes_file)
        {
            return Ok(None);
        }
        if selector_omitted {
            // Add only the omitted selector; existing values, inline comments
            // and decor stay exact.
            doc.insert("tools", toml_value_to_edit_item(&omitted_tool_selection()));
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
    // Generation creates a new installation, even when the requested schema
    // is historical. Write its full default selection so the document states
    // it and `config migrate` has nothing to normalize.
    let tools = toml::Value::try_from(crate::builtin_tools::BuiltinToolsConfig::default())
        .context("failed to serialize default tool selection")?;
    value
        .as_table_mut()
        .context("generated config root is not a TOML table")?
        .insert("tools".to_string(), tools);
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
    let kept_original = if from < CURRENT_SCHEMA_VERSION {
        keep_version_backup(path, from)?
    } else {
        None
    };
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
        version_backup_path: kept_original,
        from_version: from,
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

/// Keep a complete copy of `config_path`, as it is now, beside it, and return
/// the copy's path when this call wrote it. A kept copy is never replaced or
/// removed, so the original of each version survives every later migration
/// and save. Because it lasts and can hold the plaintext secrets of keys the
/// migration retires, it is owner-only (0600 on Unix) whatever the mode of the
/// file it copies.
///
/// The copy is written and synced under a private name, then published with a
/// hard link at [`version_backup_path`], which never replaces an existing
/// path. So a file there is never an unfinished copy of this binary's, and
/// one counts as the copy when its bytes equal the file about to be
/// rewritten; it is then synced, and made owner-only if an older build left
/// it readable by others. Anything else there (a different revision, or a
/// partial copy an older build left) is left exactly as it is, and the copy
/// goes to the first free numbered name, `<name>.v<version>-<n>.backup`. A
/// path that is not a regular file is refused rather than skipped.
///
/// Where hard links are not available, the synced private copy is renamed to
/// a name of its own, `<name>.v<version>-<uuid>.backup`, which no other writer
/// takes for its copy. So no writer counts on a copy another may still fail to
/// finish, and a failure removes nothing another writer relies on.
pub fn keep_version_backup(config_path: &Path, version: u32) -> Result<Option<std::path::PathBuf>> {
    let original = std::fs::read(config_path).with_context(|| {
        format!(
            "failed to read {} to keep a copy before rewriting it",
            config_path.display()
        )
    })?;
    let mut private: Option<PrivateCopy> = None;
    for n in 0..MAX_VERSION_BACKUPS {
        let candidate = numbered_version_backup_path(config_path, version, n)?;
        match kept_copy_at(&candidate, &original)? {
            KeptCopy::Same(kept) => {
                confirm_kept_copy(&candidate, &kept)?;
                return Ok(None);
            }
            KeptCopy::Other => continue,
            KeptCopy::Absent => {}
        }
        let copy = match private.take() {
            Some(copy) => copy,
            None => PrivateCopy::write(config_path, version, &original)?,
        };
        match hard_link_for_backup(&copy.path, &candidate) {
            Ok(()) => {
                sync_parent(&candidate).with_context(|| backup_failed(config_path, &candidate))?;
                return Ok(Some(candidate));
            }
            // Another writer published here first. Its copy is complete:
            // it is the original if the bytes match, else another revision.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                private = Some(copy);
                if let KeptCopy::Same(kept) = kept_copy_at(&candidate, &original)? {
                    confirm_kept_copy(&candidate, &kept)?;
                    return Ok(None);
                }
            }
            Err(_) => return publish_under_own_name(config_path, version, copy).map(Some),
        }
    }
    anyhow::bail!(
        "{} other copies of {} already sit beside it as `.v{version}` backups; \
         move the old ones aside before saving over the config",
        MAX_VERSION_BACKUPS,
        config_path.display()
    )
}

/// How many numbered `.v<version>` backups `keep_version_backup` tries.
const MAX_VERSION_BACKUPS: u32 = 100;

/// [`version_backup_path`] for `n == 0`, else `<name>.v<version>-<n>.backup`.
fn numbered_version_backup_path(
    config_path: &Path,
    version: u32,
    n: u32,
) -> Result<std::path::PathBuf> {
    if n == 0 {
        return version_backup_path(config_path, version);
    }
    let file_name = config_file_name(config_path)?;
    Ok(config_path.with_file_name(format!("{file_name}.v{version}-{n}.backup")))
}

fn config_file_name(config_path: &Path) -> Result<&str> {
    config_path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("config path {} has no file name", config_path.display()))
}

fn backup_failed(config_path: &Path, target: &Path) -> String {
    format!(
        "failed to keep a copy of {} as {} before rewriting it",
        config_path.display(),
        target.display()
    )
}

/// What a backup path holds, compared with the bytes about to be rewritten.
enum KeptCopy {
    Absent,
    /// A regular file with exactly those bytes, opened: the copy is kept.
    Same(std::fs::File),
    /// A regular file with other bytes. It is left as it is.
    Other,
}

fn kept_copy_at(path: &Path, original: &[u8]) -> Result<KeptCopy> {
    use std::io::Read as _;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(KeptCopy::Absent),
        Err(err) => {
            return Err(anyhow::Error::from(err))
                .with_context(|| format!("failed to check {}", path.display()));
        }
    };
    if !meta.file_type().is_file() {
        anyhow::bail!(
            "{} exists but is not a regular file, so it cannot be a kept pre-upgrade \
             original; move it aside before saving over the config",
            path.display()
        );
    }
    let mut file =
        std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = file
            .metadata()
            .with_context(|| format!("failed to check {}", path.display()))?;
        if (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
            anyhow::bail!(
                "{} changed while it was being checked; retry the save",
                path.display()
            );
        }
    }
    let mut held = Vec::new();
    file.read_to_end(&mut held)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(if held == original {
        KeptCopy::Same(file)
    } else {
        KeptCopy::Other
    })
}

/// Make a copy already kept at `path` as safe as one this call would write
/// before counting on it: owner-only, and synced with its directory entry. A
/// copy readable by others that cannot be made owner-only is refused, as the
/// save would otherwise leave its secrets exposed.
fn confirm_kept_copy(path: &Path, kept: &std::fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = kept
            .metadata()
            .with_context(|| format!("failed to check {}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            kept.set_permissions(std::fs::Permissions::from_mode(0o600))
                .with_context(|| {
                    format!(
                        "{} can be read by others and could not be made owner-only; \
                         restrict it (chmod 600) or move it aside before saving over the config",
                        path.display()
                    )
                })?;
        }
        kept.sync_all()
            .with_context(|| format!("failed to sync {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = kept;
    sync_parent(path).with_context(|| format!("failed to sync the directory of {}", path.display()))
}

/// A synced, owner-only copy of the config under a private name, removed
/// when dropped. Publishing links or renames it into place.
struct PrivateCopy {
    path: std::path::PathBuf,
}

impl PrivateCopy {
    fn write(config_path: &Path, version: u32, original: &[u8]) -> Result<Self> {
        use std::io::Write as _;
        let file_name = config_file_name(config_path)?;
        let path = config_path.with_file_name(format!(
            ".{file_name}.v{version}.backup.tmp-{}",
            uuid::Uuid::new_v4()
        ));
        let failed = || backup_failed(config_path, &path);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).with_context(failed)?;
        let copy = Self { path: path.clone() };
        #[cfg(test)]
        tests::run_version_backup_hook(config_path, tests::BackupStage::Copying);
        file.write_all(original)
            .and_then(|()| file.sync_all())
            .with_context(failed)?;
        Ok(copy)
    }
}

impl Drop for PrivateCopy {
    fn drop(&mut self) {
        // Gone already once it is renamed into place.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where hard links are not available: rename the synced private copy to a
/// name of its own. The name is new, so the rename replaces nothing, and no
/// other writer counts on this copy, so a failure here leaves nothing anyone
/// relies on.
fn publish_under_own_name(
    config_path: &Path,
    version: u32,
    copy: PrivateCopy,
) -> Result<std::path::PathBuf> {
    let file_name = config_file_name(config_path)?;
    let own = config_path.with_file_name(format!(
        "{file_name}.v{version}-{}.backup",
        uuid::Uuid::new_v4()
    ));
    #[cfg(test)]
    {
        tests::run_version_backup_hook(config_path, tests::BackupStage::Publishing);
        tests::run_version_backup_hook(config_path, tests::BackupStage::FallbackWritten);
        if tests::fallback_commit_fails() {
            return Err(anyhow::Error::from(std::io::Error::other(
                "injected failure committing the fallback copy",
            )))
            .with_context(|| backup_failed(config_path, &own));
        }
    }
    std::fs::rename(&copy.path, &own).with_context(|| backup_failed(config_path, &own))?;
    sync_parent(&own).with_context(|| backup_failed(config_path, &own))?;
    Ok(own)
}

/// [`sync_directory`] on the directory holding `path`.
fn sync_parent(path: &Path) -> Result<()> {
    match path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        Some(dir) => sync_directory(dir),
        None => Ok(()),
    }
}

/// `std::fs::hard_link`, which a test can make fail to exercise the fallback.
fn hard_link_for_backup(original: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        if tests::hard_links_fail_in(link) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "hard links are disabled for this test",
            ));
        }
    }
    std::fs::hard_link(original, link)
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
/// migration or compatibility normalization ran (vs. `Ok(None)` when neither
/// the schema nor the persisted selector needs updating).
#[derive(Debug, Clone)]
pub struct MigrateReport {
    pub backup_path: std::path::PathBuf,
    /// The lasting owner-only copy of the pre-upgrade original
    /// (`<name>.v<version>.backup`), when this migration wrote it. `None` when
    /// the file was already current or an earlier migration kept that copy.
    pub version_backup_path: Option<std::path::PathBuf>,
    pub from_version: u32,
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
        for pattern in spellings_of(key.path) {
            for concrete in expand_doc_path(root, &pattern) {
                let segments: Vec<&str> = concrete.iter().map(String::as_str).collect();
                if take_doc_path(root, &segments, &pattern).is_none() {
                    continue;
                }
                notices.push(MigrationNotice::Removed {
                    path: segments.join("."),
                    reason: key.reason,
                });
            }
        }
    }
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
        // Sorted, as `expand_path` walks a sorted table, so both appliers
        // retire wildcard matches, and report them, in the same order.
        let keys: Vec<String> = if *segment == ANY_KEY {
            let mut keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
            keys.sort();
            keys
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
            MigrationNotice::AssumedV1
            | MigrationNotice::InferredV3
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
        for pattern in spellings_of(key.path) {
            for concrete in expand_path(root, &pattern) {
                let segments: Vec<&str> = concrete.iter().map(String::as_str).collect();
                if take_path(root, &segments, &pattern).is_none() {
                    continue;
                }
                notices.push(MigrationNotice::Removed {
                    path: segments.join("."),
                    reason: key.reason,
                });
            }
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
            // Sorted whatever the map's own order, as `expand_doc_path` sorts.
            let mut keys: Vec<&String> = table.keys().collect();
            keys.sort();
            keys
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

    /// A test-only stand-in for a crash or a second writer inside
    /// `keep_version_backup`, once its file exists and before the copy is
    /// complete. Keyed by the config's directory so other tests are unaffected.
    /// Where in `keep_version_backup` the hook runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum BackupStage {
        /// The private copy exists and is not yet complete.
        Copying,
        /// The copy is complete, hard-linking failed, and the fallback is
        /// about to publish it.
        Publishing,
        /// Hard-linking failed and the fallback copy holds every byte, but is
        /// not yet committed.
        FallbackWritten,
    }

    /// The writer thread whose fallback commit fails, set by a test's hook.
    static FAILING_FALLBACK_COMMIT: std::sync::Mutex<Option<std::thread::ThreadId>> =
        std::sync::Mutex::new(None);

    pub(super) fn fallback_commit_fails() -> bool {
        *FAILING_FALLBACK_COMMIT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            == Some(std::thread::current().id())
    }

    fn set_failing_fallback_commit(writer: Option<std::thread::ThreadId>) {
        *FAILING_FALLBACK_COMMIT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = writer;
    }

    type BackupHook = std::sync::Arc<dyn Fn(BackupStage) + Send + Sync>;
    static VERSION_BACKUP_HOOK: std::sync::Mutex<Option<(std::path::PathBuf, BackupHook)>> =
        std::sync::Mutex::new(None);
    /// Serializes the tests that install the hook.
    static VERSION_BACKUP_HOOK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Config directories whose backups take the no-hard-link fallback.
    static NO_HARD_LINKS: std::sync::Mutex<Vec<std::path::PathBuf>> =
        std::sync::Mutex::new(Vec::new());

    pub(super) fn hard_links_fail_in(link: &Path) -> bool {
        let dirs = NO_HARD_LINKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        link.parent()
            .is_some_and(|dir| dirs.iter().any(|d| d == dir))
    }

    fn set_no_hard_links(dir: &Path, on: bool) {
        let mut dirs = NO_HARD_LINKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dirs.retain(|d| d != dir);
        if on {
            dirs.push(dir.to_path_buf());
        }
    }

    pub(super) fn run_version_backup_hook(config_path: &Path, stage: BackupStage) {
        let hook = VERSION_BACKUP_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|(dir, _)| config_path.parent() == Some(dir.as_path()))
            .map(|(_, hook)| hook.clone());
        if let Some(hook) = hook {
            hook(stage);
        }
    }

    fn set_version_backup_hook(dir: &Path, hook: Option<BackupHook>) {
        *VERSION_BACKUP_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            hook.map(|hook| (dir.to_path_buf(), hook));
    }

    const V2_BACKUP_ORIGINAL: &str =
        "schema_version = 2\n\n[identity]\nformat = \"openclaw\"\n\n[gateway]\nport = 42617\n";

    /// A process killed while the pre-upgrade copy is being written must not
    /// leave something a later run takes for the finished copy.
    #[test]
    fn v4_review_interrupted_version_backup_cannot_authorize_rewrite() {
        let _serial = VERSION_BACKUP_HOOK_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V2_BACKUP_ORIGINAL).unwrap();

        fn crash(stage: BackupStage) {
            if stage == BackupStage::Copying {
                panic!("simulated crash mid-backup");
            }
        }
        set_version_backup_hook(dir.path(), Some(std::sync::Arc::new(crash) as BackupHook));
        let crashed = std::panic::catch_unwind(|| migrate_file_in_place(&path));
        set_version_backup_hook(dir.path(), None);
        assert!(crashed.is_err(), "the failpoint must fire");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            V2_BACKUP_ORIGINAL,
            "nothing is rewritten before the copy is kept"
        );

        let retried = migrate_file_in_place(&path);
        let backup = version_backup_path(&path, 2).unwrap();
        match retried {
            Ok(_) => assert_eq!(
                std::fs::read_to_string(&backup).unwrap(),
                V2_BACKUP_ORIGINAL,
                "a retry that rewrites the file must leave a complete original"
            ),
            Err(_) => assert_eq!(std::fs::read_to_string(&path).unwrap(), V2_BACKUP_ORIGINAL),
        }
    }

    /// A second writer must not take another writer's unfinished copy for a
    /// kept original.
    #[test]
    fn v4_review_concurrent_version_backup_waits_for_a_complete_copy() {
        let _serial = VERSION_BACKUP_HOOK_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V2_BACKUP_ORIGINAL).unwrap();

        let (reached_tx, reached_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let reached_tx = std::sync::Mutex::new(Some(reached_tx));
        let go_rx = std::sync::Mutex::new(go_rx);
        set_version_backup_hook(
            dir.path(),
            Some(std::sync::Arc::new(move |stage| {
                if stage != BackupStage::Copying {
                    return;
                }
                // Only the first writer pauses. Take the sender in its own
                // statement so the lock is released before it blocks.
                let first = reached_tx.lock().unwrap().take();
                if let Some(tx) = first {
                    tx.send(()).unwrap();
                    let _ = go_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(30));
                }
            }) as BackupHook),
        );
        let first = {
            let path = path.clone();
            std::thread::spawn(move || keep_version_backup(&path, 2))
        };
        reached_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the first writer reaches the failpoint");
        let second = keep_version_backup(&path, 2);
        let backup = version_backup_path(&path, 2).unwrap();
        let seen_by_second = std::fs::read_to_string(&backup).unwrap_or_default();
        go_tx.send(()).unwrap();
        let first = first.join().unwrap();
        set_version_backup_hook(dir.path(), None);

        assert!(second.is_ok(), "{second:?}");
        assert_eq!(
            seen_by_second, V2_BACKUP_ORIGINAL,
            "when the second writer returns, the kept original must be complete"
        );
        assert!(first.is_ok(), "{first:?}");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            V2_BACKUP_ORIGINAL
        );
    }

    /// The copies of the config kept as `config.toml.v2…backup`, any numbering.
    fn kept_v2_copies(dir: &Path) -> Vec<String> {
        let mut copies: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with("config.toml.v2") && name.ends_with(".backup")
            })
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .collect();
        copies.sort();
        copies
    }

    /// Where hard links are missing, a writer publishing its copy must never
    /// replace one another writer already published.
    #[test]
    fn v4_review_r2_backup_fallback_never_replaces_winner() {
        const EARLIER: &str = "schema_version = 2\n\n[gateway]\nport = 1\n";
        const LATER: &str = "schema_version = 2\n\n[gateway]\nport = 2\n";
        let _serial = VERSION_BACKUP_HOOK_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, EARLIER).unwrap();
        set_no_hard_links(dir.path(), true);

        let (reached_tx, reached_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let reached_tx = std::sync::Mutex::new(Some(reached_tx));
        let go_rx = std::sync::Mutex::new(go_rx);
        set_version_backup_hook(
            dir.path(),
            Some(std::sync::Arc::new(move |stage| {
                if stage != BackupStage::Publishing {
                    return;
                }
                let first = reached_tx.lock().unwrap().take();
                if let Some(tx) = first {
                    tx.send(()).unwrap();
                    let _ = go_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(30));
                }
            }) as BackupHook),
        );
        // The first writer copies EARLIER and pauses just before publishing.
        let first = {
            let path = path.clone();
            std::thread::spawn(move || keep_version_backup(&path, 2))
        };
        reached_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the first writer reaches the publish point");
        // The file changes, and a second writer copies and publishes LATER.
        std::fs::write(&path, LATER).unwrap();
        let second = keep_version_backup(&path, 2);
        go_tx.send(()).unwrap();
        let first = first.join().unwrap();
        set_version_backup_hook(dir.path(), None);
        set_no_hard_links(dir.path(), false);

        assert!(second.is_ok(), "{second:?}");
        let kept = kept_v2_copies(dir.path());
        assert!(
            kept.contains(&LATER.to_string()),
            "the copy published first is never replaced: {kept:?}"
        );
        if first.is_ok() {
            assert!(
                kept.contains(&EARLIER.to_string()),
                "the other snapshot is kept too: {kept:?}"
            );
        }
    }

    /// A real incremental save over the V2 config at `path`, as the daemon
    /// runs one.
    fn save_dirty_over(path: &Path) -> Result<()> {
        let mut config = migrate_to_current(&std::fs::read_to_string(path).unwrap())
            .expect("the V2 file loads, migrated in memory");
        config.config_path = path.to_path_buf();
        config.loaded_from = Some(path.to_path_buf());
        config.observability.backend = crate::schema::ObservabilityBackend::Otel;
        config.mark_dirty("observability.backend");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(config.save_dirty())
    }

    /// Where hard links are missing, a copy is not kept until it is complete
    /// and committed. Another writer must not count it as kept before then:
    /// if the commit then fails and the copy is removed, a config that writer
    /// rewrote is left with no original.
    #[test]
    fn v4_review_r3_fallback_copy_cannot_be_accepted_before_commit() {
        let _serial = VERSION_BACKUP_HOOK_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V2_BACKUP_ORIGINAL).unwrap();
        set_no_hard_links(dir.path(), true);

        let (reached_tx, reached_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let reached_tx = std::sync::Mutex::new(Some(reached_tx));
        let go_rx = std::sync::Mutex::new(go_rx);
        set_version_backup_hook(
            dir.path(),
            Some(std::sync::Arc::new(move |stage| {
                if stage != BackupStage::FallbackWritten {
                    return;
                }
                let first = reached_tx.lock().unwrap().take();
                if let Some(tx) = first {
                    // This writer's commit fails once it resumes.
                    set_failing_fallback_commit(Some(std::thread::current().id()));
                    tx.send(()).unwrap();
                    let _ = go_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(30));
                }
            }) as BackupHook),
        );
        // Writer A writes its copy and pauses before committing it.
        let first = {
            let path = path.clone();
            std::thread::spawn(move || keep_version_backup(&path, 2))
        };
        reached_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the first writer reaches the commit point");
        // Writer B saves over the config meanwhile.
        let saved = save_dirty_over(&path);
        go_tx.send(()).unwrap();
        let first = first.join().unwrap();
        set_version_backup_hook(dir.path(), None);
        set_failing_fallback_commit(None);
        set_no_hard_links(dir.path(), false);

        assert!(first.is_err(), "the injected commit failure must fire");
        match saved {
            Ok(()) => assert!(
                kept_v2_copies(dir.path()).contains(&V2_BACKUP_ORIGINAL.to_string()),
                "the save rewrote the config, so a complete original must survive both \
                 writers; got {:?}",
                kept_v2_copies(dir.path())
            ),
            Err(_) => assert_eq!(std::fs::read_to_string(&path).unwrap(), V2_BACKUP_ORIGINAL),
        }
    }

    /// A writer killed before its fallback copy is committed must not leave
    /// something a later save takes for the original.
    #[test]
    fn v4_review_r3_interrupted_fallback_copy_is_never_trusted() {
        let _serial = VERSION_BACKUP_HOOK_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V2_BACKUP_ORIGINAL).unwrap();
        set_no_hard_links(dir.path(), true);

        fn crash(stage: BackupStage) {
            if stage == BackupStage::FallbackWritten {
                panic!("simulated crash before the fallback copy is committed");
            }
        }
        set_version_backup_hook(dir.path(), Some(std::sync::Arc::new(crash) as BackupHook));
        let crashed = std::panic::catch_unwind(|| keep_version_backup(&path, 2));
        set_version_backup_hook(dir.path(), None);
        let saved = save_dirty_over(&path);
        set_no_hard_links(dir.path(), false);

        assert!(crashed.is_err(), "the failpoint must fire");
        match saved {
            Ok(()) => assert!(
                kept_v2_copies(dir.path()).contains(&V2_BACKUP_ORIGINAL.to_string()),
                "a save after the crash must leave a complete original; got {:?}",
                kept_v2_copies(dir.path())
            ),
            Err(_) => assert_eq!(std::fs::read_to_string(&path).unwrap(), V2_BACKUP_ORIGINAL),
        }
    }

    /// Unversioned files whose sections mix the V3 layout with older ones are
    /// refused rather than read as V1, which would reshape the V3 sections.
    #[test]
    fn v4_review_mixed_unversioned_shapes_are_refused() {
        for raw in [
            format!("default_model = \"legacy\"\n{HAND_WRITTEN_V3_WITHOUT_VERSION}"),
            format!("{HAND_WRITTEN_V3_WITHOUT_VERSION}\n[swarms.old]\nstrategy = \"sequential\"\n"),
            "[providers.models.openai.work]\nmodel = \"gpt-4o\"\n\n[channels.discord]\nbot_token = \"t\"\n"
                .to_string(),
        ] {
            let v: toml::Value = toml::from_str(&raw).unwrap();
            assert!(detect_version(&v).is_err(), "{raw}");
            assert!(migrate_file_with_notices(&raw).is_err(), "{raw}");
            let dir = setup_temp_config_dir();
            let path = dir.path().join("config.toml");
            std::fs::write(&path, &raw).unwrap();
            assert!(migrate_file_in_place(&path).is_err(), "{raw}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        }
    }

    /// The parsed-value and document appliers report the same removals in the
    /// same order, whatever order the aliases appear in the file.
    #[test]
    fn v4_review_appliers_agree_on_order() {
        let raw = "schema_version = 4\n\n[agents.z]\ncompact_context = true\n\n[agents.a]\ncompact_context = true\n";
        let mut value: toml::Value = toml::from_str(raw).unwrap();
        let mut value_notices = Vec::new();
        for version in 2..=CURRENT_SCHEMA_VERSION {
            apply_retired_keys(&mut value, version, RETIRED_KEYS, &mut value_notices);
        }
        let (out, doc_notices) = apply_doc(raw);
        assert_eq!(value_notices, doc_notices);
        let from_doc: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(value, from_doc);
    }

    // Existing-document compatibility and explicit selector intent.
    #[test]
    fn compatibility_omission_is_shared_by_all_migration_routes() {
        let current = format!("schema_version = {CURRENT_SCHEMA_VERSION}\n");
        for raw in [
            "default_model_provider = \"openai\"\ndefault_model = \"test-model\"\n",
            "schema_version = 2\n",
            "schema_version = 3\n",
            current.as_str(),
        ] {
            let (value, _) = migrate_value(raw).unwrap();
            assert_eq!(
                value["tools"]["optional"],
                toml::Value::Array(vec![toml::Value::String("*".into())])
            );
            assert_eq!(migrate_to_current(raw).unwrap().tools.optional, ["*"]);
            assert_eq!(migrate_to_current_resilient(raw).tools.optional, ["*"]);
            let migrated = migrate_file(raw)
                .unwrap()
                .expect("omission needs normalization");
            assert_eq!(migrate_to_current(&migrated).unwrap().tools.optional, ["*"]);
            assert!(migrate_file(&migrated).unwrap().is_none());
        }
    }

    #[test]
    fn compatibility_preserves_explicit_sections_before_legacy_serde() {
        let current = format!("schema_version = {CURRENT_SCHEMA_VERSION}\n");
        for prefix in [
            "",
            "schema_version = 2\n",
            "schema_version = 3\n",
            current.as_str(),
        ] {
            for section in [
                "[tools]\n",
                "[tools]\noptional = []\n",
                "[tools]\noptional = [\"calculator\"]\n",
                "[tools]\noptional = [\"*\"]\n",
                "[tools]\noptional = [\"not-a-real-tool\"]\n",
            ] {
                let raw = format!("{prefix}{section}");
                let original: toml::Value = toml::from_str(&raw).unwrap();
                let (migrated, _) = migrate_value(&raw).unwrap();
                assert_eq!(migrated.get("tools"), original.get("tools"), "{raw}");
                let strict = migrate_to_current(&raw).unwrap();
                let resilient = migrate_to_current_resilient(&raw);
                assert_eq!(strict.tools.optional, resilient.tools.optional);
                // A section without `optional` takes the full default.
                let full = section == "[tools]\n" || section.contains("\"*\"");
                assert_eq!(strict.tools.is_enabled("weather"), full, "{raw}");
                if let Some(disk) = migrate_file(&raw).unwrap() {
                    let written: toml::Value = toml::from_str(&disk).unwrap();
                    assert_eq!(written.get("tools"), original.get("tools"));
                }
            }
        }
    }

    #[test]
    fn compatibility_malformed_tools_are_refused_then_salvaged_to_the_default() {
        let current = format!("schema_version = {CURRENT_SCHEMA_VERSION}\n");
        for prefix in [
            "",
            "schema_version = 2\n",
            "schema_version = 3\n",
            current.as_str(),
        ] {
            for section in [
                "tools = false\n",
                "[tools]\noptional = 42\n",
                "[tools]\noptional = [false]\n",
                "[tools]\nunknown_selector = true\n",
            ] {
                let raw = format!("{prefix}{section}");
                let original: toml::Value = toml::from_str(&raw).unwrap();
                assert_eq!(
                    migrate_value(&raw).unwrap().0.get("tools"),
                    original.get("tools")
                );
                assert!(migrate_to_current(&raw).is_err());
                let salvaged = migrate_to_current_salvaged(&raw);
                assert_eq!(salvaged.config.tools.optional, ["*"]);
                assert!(salvaged.dropped.iter().any(|path| path == "tools"));
            }
        }
    }

    #[test]
    fn current_schema_normalization_preserves_comments_backup_and_noop() {
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        let raw = format!(
            "schema_version = {CURRENT_SCHEMA_VERSION} # version comment\n# locale comment\nlocale = \"en\" # inline comment\n"
        );
        let raw = raw.as_str();
        std::fs::write(&path, raw).unwrap();
        let report = migrate_file_in_place(&path)
            .unwrap()
            .expect("normalize omission");
        assert_eq!(report.to_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(std::fs::read_to_string(&report.backup_path).unwrap(), raw);
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.starts_with(raw),
            "original bytes and comments must stay: {written}"
        );
        assert_eq!(migrate_to_current(&written).unwrap().tools.optional, ["*"]);
        assert!(migrate_file_in_place(&path).unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), written);
        assert_eq!(std::fs::read_to_string(&report.backup_path).unwrap(), raw);
    }

    #[test]
    fn compatibility_normalization_changes_only_availability() {
        let raw = format!(
            "schema_version = {CURRENT_SCHEMA_VERSION}\n[plugins]\nenabled = false\nauto_discover = false\n[risk_profiles.limited]\nallowed_tools = [\"file_read\"]\n"
        );
        let raw = raw.as_str();
        let original: toml::Value = toml::from_str(raw).unwrap();
        let (migrated, _) = migrate_value(raw).unwrap();
        for (key, value) in original.as_table().unwrap() {
            assert_eq!(migrated.get(key), Some(value), "changed {key}");
        }
        let config = migrate_to_current(raw).unwrap();
        assert!(!config.plugins.enabled);
        assert!(!config.plugins.auto_discover);
        assert_eq!(config.risk_profiles["limited"].allowed_tools, ["file_read"]);
    }

    #[test]
    fn current_explicit_selector_is_noop_even_with_unrelated_nan() {
        let raw = format!(
            "schema_version = {CURRENT_SCHEMA_VERSION}\nmetric = nan\n[tools]\noptional = []\n"
        );
        assert!(migrate_file(&raw).unwrap().is_none());
    }

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
    fn migrate_file_in_place_noop_when_current_and_selector_is_explicit() {
        let dir = setup_temp_config_dir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!("schema_version = {CURRENT_SCHEMA_VERSION}\n[tools]\noptional = []\n"),
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
        // The tool selector is stated, so there is nothing to normalize either.
        let raw =
            format!("schema_version = {CURRENT_SCHEMA_VERSION}\n[tools]\noptional = [\"*\"]\n");
        assert_eq!(migrate_file_with_notices(&raw).unwrap(), None);
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
            // The V2 cron layout.
            "[cron]\nenabled = true\n\n[[cron.jobs]]\nname = \"nightly\"\n",
        ] {
            let v: toml::Value = toml::from_str(raw).unwrap();
            assert_eq!(detect_version(&v).unwrap(), 1, "{raw}");
            let (_, notices) = migrate_file_with_notices(raw).unwrap().unwrap();
            assert_eq!(notices.first(), Some(&MigrationNotice::AssumedV1), "{raw}");
        }
    }

    #[test]
    fn a_v1_only_key_keeps_the_v1_reading() {
        // Beside V3 sections the same key makes the file ambiguous; see
        // `v4_review_mixed_unversioned_shapes_are_refused`.
        let raw = "default_model = \"gpt-4\"\n\n[gateway]\nport = 42617\n";
        let v: toml::Value = toml::from_str(raw).unwrap();
        assert_eq!(detect_version(&v).unwrap(), 1);
    }

    #[test]
    fn keys_shared_with_the_current_schema_do_not_veto_the_inference() {
        let raw =
            format!("[cron.nightly]\nschedule = \"0 0 * * *\"\n{HAND_WRITTEN_V3_WITHOUT_VERSION}");
        let v: toml::Value = toml::from_str(&raw).unwrap();
        assert_eq!(detect_version(&v).unwrap(), INFERRED_SCHEMA_VERSION);

        // A declarative job is V3 evidence on its own, without provider or
        // channel sections beside it.
        let raw = "[cron.nightly]\ncommand = \"echo keep\"\nschedule = { kind = \"cron\", expr = \"0 0 * * *\" }\n";
        let v: toml::Value = toml::from_str(raw).unwrap();
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

    /// The catalog names each top-level setting `V1Config` reads and each
    /// spelling it accepts for one, and none of them is a current section.
    /// `model_routes` and `embedding_routes` are current sections too, told
    /// apart by a route naming `provider`; `channels_config` is a current
    /// spelling of `channels`, told apart by its shape.
    #[test]
    fn the_legacy_catalog_names_every_v1_setting() {
        let current = current_top_level_sections();
        let top_level: Vec<&str> = LEGACY_LAYOUT_PATHS
            .iter()
            .filter(|path| path.len() == 1)
            .map(|path| path[0])
            .collect();
        for key in &top_level {
            assert!(!current.contains(*key), "`{key}` is a current section");
        }

        let some = || Some(toml::Value::String("x".into()));
        let table = || {
            toml::Value::Table(toml::Table::from_iter([(
                "k".to_string(),
                toml::Value::String("x".into()),
            )]))
        };
        let every_v1_setting = V1Config {
            api_key: some(),
            api_url: some(),
            api_path: some(),
            default_provider: some(),
            default_model: some(),
            model_providers: std::collections::HashMap::from([("ollama".to_string(), table())]),
            default_temperature: some(),
            provider_timeout_secs: some(),
            provider_max_tokens: some(),
            extra_headers: std::collections::HashMap::from([(
                "X".to_string(),
                toml::Value::String("x".into()),
            )]),
            model_routes: vec![table()],
            embedding_routes: vec![table()],
            channels_config: Some(table()),
            passthrough: toml::Table::new(),
        };
        let written = toml::Value::try_from(every_v1_setting).unwrap();
        for key in written.as_table().unwrap().keys() {
            assert!(
                top_level.contains(&key.as_str())
                    || ["model_routes", "embedding_routes", "channels_config"]
                        .contains(&key.as_str()),
                "`{key}` is a V1 setting the catalog does not name"
            );
        }
        for alias in ["model_provider", "model"] {
            let value: toml::Value = toml::from_str(&format!("{alias} = \"x\"\n")).unwrap();
            let v1: V1Config = value.try_into().unwrap();
            assert!(v1.passthrough.is_empty(), "`{alias}` is not a V1Config key");
            assert!(
                top_level.contains(&alias),
                "`{alias}` is not in the catalog"
            );
        }
    }

    /// Every path of the catalog has its example, so each is pinned by
    /// `every_legacy_layout_example_is_read_as_v1_and_refused_beside_v3`, as
    /// are the V2 cron knobs and V2-only agent keys.
    #[test]
    fn every_legacy_layout_path_has_an_example() {
        let examples: Vec<&str> = LEGACY_LAYOUT_EXAMPLES
            .iter()
            .map(|(path, _)| *path)
            .collect();
        let required = LEGACY_LAYOUT_PATHS
            .iter()
            .map(|path| path.join("."))
            .chain(V2_CRON_KNOBS.iter().map(|knob| format!("cron.{knob}")))
            .chain(
                V2_ONLY_AGENT_KEYS
                    .iter()
                    .map(|key| format!("agents.coder.{key}")),
            );
        for path in required {
            assert!(examples.contains(&path.as_str()), "`{path}` has no example");
        }
    }

    // ── The layouts before V3, and files with no version ────────────

    /// The value at a dotted path, or `None`.
    fn value_at<'a>(value: &'a toml::Value, dotted: &str) -> Option<&'a toml::Value> {
        dotted
            .split('.')
            .try_fold(value, |value, key| value.get(key))
    }

    /// One file per way an older layout differs from the current one, each
    /// with the path a refusal names.
    const LEGACY_LAYOUT_EXAMPLES: &[(&str, &str)] = &[
        ("api_key", "api_key = \"sk-v1\"\n"),
        ("api_url", "api_url = \"https://llm.example.invalid/v1\"\n"),
        ("api_path", "api_path = \"/v1/chat/completions\"\n"),
        ("default_provider", "default_provider = \"ollama\"\n"),
        ("model_provider", "model_provider = \"ollama\"\n"),
        ("default_model", "default_model = \"llama3\"\n"),
        ("model", "model = \"llama3\"\n"),
        (
            "model_providers",
            "[model_providers.ollama]\nmodel = \"llama3\"\n",
        ),
        ("default_temperature", "default_temperature = 0.4\n"),
        ("provider_timeout_secs", "provider_timeout_secs = 30\n"),
        ("provider_max_tokens", "provider_max_tokens = 2048\n"),
        ("extra_headers", "[extra_headers]\nX-Trace = \"v1\"\n"),
        (
            "channels_config",
            "[channels_config.telegram]\nbot_token = \"t\"\n",
        ),
        ("autonomy", "[autonomy]\nmax_actions_per_hour = 7\n"),
        ("agent", "[agent]\nmax_tool_iterations = 9\n"),
        ("swarms", "[swarms.old]\nstrategy = \"sequential\"\n"),
        ("identity", "[identity]\nformat = \"openclaw\"\n"),
        ("node_transport", "[node_transport]\nenabled = true\n"),
        ("security.sandbox", "[security.sandbox]\nenabled = true\n"),
        (
            "security.resources",
            "[security.resources]\nmax_memory_mb = 512\n",
        ),
        (
            "reliability.fallback_providers",
            "[reliability]\nfallback_providers = [\"openai\"]\n",
        ),
        (
            "reliability.model_fallbacks",
            "[reliability.model_fallbacks]\ngpt-4o = [\"gpt-4o-mini\"]\n",
        ),
        (
            "memory.qdrant",
            "[memory.qdrant]\nurl = \"http://localhost:6333\"\n",
        ),
        (
            "memory.postgres",
            "[memory.postgres]\nvector_enabled = true\n",
        ),
        (
            "memory.sqlite_open_timeout_secs",
            "[memory]\nsqlite_open_timeout_secs = 5\n",
        ),
        (
            "storage.provider",
            "[storage.provider.config]\nprovider = \"sqlite\"\n\
             db_url = \"sqlite:///tmp/review-memory.db\"\nconnect_timeout_secs = 17\n",
        ),
        (
            "cost.prices",
            "[cost.prices.\"openai/gpt-4o\"]\ninput = 2.5\noutput = 10.0\n",
        ),
        ("tts.openai", "[tts.openai]\nvoice = \"alloy\"\n"),
        (
            "tts.elevenlabs",
            "[tts.elevenlabs]\nmodel_id = \"eleven_v2\"\n",
        ),
        ("tts.google", "[tts.google]\nlanguage_code = \"en-US\"\n"),
        ("tts.edge", "[tts.edge]\nbinary_path = \"edge-tts\"\n"),
        (
            "tts.piper",
            "[tts.piper]\napi_url = \"http://localhost:5000\"\n",
        ),
        (
            "tts.default_provider",
            "[tts]\ndefault_provider = \"openai\"\n",
        ),
        (
            "transcription.default_provider",
            "[transcription]\ndefault_provider = \"groq\"\n",
        ),
        (
            "transcription.default_model_provider",
            "[transcription]\ndefault_model_provider = \"groq\"\n",
        ),
        (
            "transcription.default_transcription_provider",
            "[transcription]\ndefault_transcription_provider = \"groq\"\n",
        ),
        (
            "gateway.pairing_dashboard.code_length",
            "[gateway.pairing_dashboard]\ncode_length = 8\n",
        ),
        ("tunnel.provider", "[tunnel]\nprovider = \"cloudflare\"\n"),
        (
            "web_search.provider",
            "[web_search]\nprovider = \"brave\"\n",
        ),
        (
            "channels.discord_history",
            "[channels.discord_history]\nbot_token = \"t\"\n",
        ),
        ("channels.feishu", "[channels.feishu]\napp_id = \"a\"\n"),
        ("providers.fallback", "[providers]\nfallback = \"openai\"\n"),
        ("providers.api_key", "[providers]\napi_key = \"sk-v2\"\n"),
        (
            "providers.extra_headers",
            "[providers.extra_headers]\nX-Trace = \"v2\"\n",
        ),
        (
            "providers.models.ollama",
            "[providers.models.ollama]\nmodel = \"llama3\"\n",
        ),
        (
            "channels.discord",
            "[channels.discord]\nbot_token = \"t\"\n",
        ),
        (
            "cron.jobs",
            "[[cron.jobs]]\nname = \"nightly\"\ncommand = \"echo\"\nschedule = \"0 0 * * *\"\n",
        ),
        ("cron.enabled", "[cron]\nenabled = true\n"),
        (
            "cron.catch_up_on_startup",
            "[cron]\ncatch_up_on_startup = true\n",
        ),
        ("cron.max_run_history", "[cron]\nmax_run_history = 10\n"),
        (
            "model_routes",
            "[[model_routes]]\nhint = \"fast\"\nprovider = \"openai\"\nmodel = \"gpt-4o-mini\"\n",
        ),
        (
            "embedding_routes",
            "[[embedding_routes]]\nhint = \"default\"\nprovider = \"openai\"\n\
             model = \"text-embedding-3-small\"\n",
        ),
        (
            "agents.coder.provider",
            "[agents.coder]\nprovider = \"openai\"\n",
        ),
        ("agents.coder.model", "[agents.coder]\nmodel = \"gpt-4o\"\n"),
        (
            "agents.coder.api_key",
            "[agents.coder]\napi_key = \"sk-agent\"\n",
        ),
        (
            "agents.coder.temperature",
            "[agents.coder]\ntemperature = 0.3\n",
        ),
        (
            "agents.coder.max_iterations",
            "[agents.coder]\nmax_iterations = 5\n",
        ),
        (
            "agents.coder.allowed_tools",
            "[agents.coder]\nallowed_tools = [\"shell\"]\n",
        ),
        ("agents.coder.agentic", "[agents.coder]\nagentic = true\n"),
        ("agents.coder.max_depth", "[agents.coder]\nmax_depth = 2\n"),
        (
            "agents.coder.skills_directory",
            "[agents.coder]\nskills_directory = \"skills\"\n",
        ),
        (
            "agents.coder.memory_namespace",
            "[agents.coder]\nmemory_namespace = \"coder\"\n",
        ),
        (
            "agents.coder.agentic_timeout_secs",
            "[agents.coder]\nagentic_timeout_secs = 60\n",
        ),
        (
            "agents.coder.timeout_secs",
            "[agents.coder]\ntimeout_secs = 30\n",
        ),
    ];

    /// [`LEGACY_LAYOUT_EXAMPLES`], and each again under every other spelling
    /// the schema accepts for its top-level section (`channels_config` for
    /// `channels`), generated from the schema's aliases.
    fn legacy_layout_examples() -> Vec<(String, String)> {
        let aliases = top_level_aliases();
        let mut examples: Vec<(String, String)> = LEGACY_LAYOUT_EXAMPLES
            .iter()
            .map(|(path, raw)| ((*path).to_string(), (*raw).to_string()))
            .collect();
        for (path, raw) in LEGACY_LAYOUT_EXAMPLES {
            let (path, raw): (&str, &str) = (path, raw);
            let (top, rest) = path.split_once('.').unwrap_or((path, ""));
            for (alias, spelled) in &aliases {
                if spelled != top {
                    continue;
                }
                let mut file: toml::Table = toml::from_str(raw).unwrap();
                let section = file.remove(top).unwrap();
                file.insert((*alias).to_string(), section);
                let path = if rest.is_empty() {
                    (*alias).to_string()
                } else {
                    format!("{alias}.{rest}")
                };
                examples.push((path, toml::to_string(&file).unwrap()));
            }
        }
        examples
    }

    /// A declarative cron job, the V3 layout, appended to a file.
    const V3_LAYOUT_PROBE: &str = "\n[cron.zz_layout_probe]\ncommand = \"echo keep\"\n\
        schedule = { kind = \"cron\", expr = \"0 0 * * *\" }\n";

    /// Each older-layout example is legacy only: the V1 -> V3 chain reads it,
    /// and the current schema does not read it where it is. So alone it
    /// keeps the V1 reading, and beside a V3 section the file is refused,
    /// naming it, as either reading would reshape part of the file.
    #[test]
    fn every_legacy_layout_example_is_read_as_v1_and_refused_beside_v3() {
        for (path, raw) in &legacy_layout_examples() {
            let (path, raw) = (path.as_str(), raw.as_str());
            let value: toml::Value =
                toml::from_str(raw).unwrap_or_else(|err| panic!("{path}: {err}"));
            assert!(
                value_at(&value, path).is_some(),
                "{path}: not in its example"
            );
            assert_eq!(
                detect_version(&value).ok(),
                Some(1),
                "{path}: an older layout keeps the V1 reading"
            );

            let mixed = format!("{raw}{V3_LAYOUT_PROBE}");
            let err = detect_version(&toml::from_str(&mixed).unwrap())
                .expect_err(&format!(
                    "{path}: beside a V3 cron job the file mixes layouts"
                ))
                .to_string();
            assert!(err.contains(path), "{path}: the refusal names it: {err}");

            let migrated = run_chain(value.clone(), 1, &mut Vec::new())
                .unwrap_or_else(|err| panic!("{path}: {err:#}"));
            assert_ne!(
                value_at(&migrated, path),
                value_at(&value, path),
                "{path}: the V1 -> V3 chain leaves it where it is"
            );
            // Strictly the current schema either refuses the file or loads
            // it without the setting in place; salvaged, it keeps it nowhere
            // either.
            let stated = format!("schema_version = 3\n{raw}");
            let (current, how) = match migrate_to_current(&stated) {
                Ok(current) => (current, "loaded strictly"),
                Err(_) => (
                    migrate_to_current_salvaged(&stated).config,
                    "refused strictly, then salvaged",
                ),
            };
            let current = toml::Value::try_from(current).unwrap();
            assert_ne!(
                value_at(&current, path),
                value_at(&value, path),
                "{path}: the current schema reads it where it is ({how}), so it is not legacy only"
            );
        }
    }

    /// Every counterexample the V4 reviews found for a file with no
    /// `schema_version`, with how it must be read.
    #[test]
    fn v4_review_unversioned_counterexamples_read_as_required() {
        #[derive(Debug, PartialEq)]
        enum Reading {
            V1,
            V3,
            Refused,
        }
        let cases: Vec<(&str, String, Reading)> = vec![
            (
                "r1: a cron job alone",
                "[cron.nightly]\ncommand = \"echo keep\"\n\
                 schedule = { kind = \"cron\", expr = \"0 0 * * *\" }\n"
                    .to_string(),
                Reading::V3,
            ),
            (
                "r1: a stale default_model beside V3 sections",
                format!("default_model = \"legacy\"\n{HAND_WRITTEN_V3_WITHOUT_VERSION}"),
                Reading::Refused,
            ),
            (
                "r1: stale swarms beside V3 sections",
                format!(
                    "{HAND_WRITTEN_V3_WITHOUT_VERSION}\n[swarms.old]\nstrategy = \"sequential\"\n"
                ),
                Reading::Refused,
            ),
            (
                "r1: a flat channel beside a provider alias",
                "[providers.models.openai.work]\nmodel = \"gpt-4o\"\n\n\
                 [channels.discord]\nbot_token = \"t\"\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "r2: flat provider fields and an alias in one section",
                "[providers.models.ollama]\nmodel = \"legacy-model\"\n\n\
                 [providers.models.ollama.work]\nmodel = \"keep-work-model\"\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "r2: flat channel fields and an alias in one section",
                "[channels.discord]\nbot_token = \"legacy-token\"\n\n\
                 [channels.discord.work]\nbot_token = \"keep-work-token\"\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "r2: V1 model_provider and model",
                "model_provider = \"ollama\"\nmodel = \"llama3\"\n".to_string(),
                Reading::V1,
            ),
            (
                "r2: V1 default_provider and model",
                "default_provider = \"ollama\"\nmodel = \"llama3\"\n".to_string(),
                Reading::V1,
            ),
            (
                "r3: an alias named extra_headers beside flat fields",
                "[providers.models.openai]\nmodel = \"legacy-model\"\n\n\
                 [providers.models.openai.extra_headers]\nmodel = \"keep-model\"\n\
                 temperature = 0.25\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "r3: an alias named pricing beside flat fields",
                "[providers.models.openai]\nmodel = \"legacy-model\"\n\n\
                 [providers.models.openai.pricing]\nmodel = \"keep-model\"\n\
                 temperature = 0.25\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "r3: the legacy storage layout",
                "[storage.provider.config]\nprovider = \"sqlite\"\n\
                 db_url = \"sqlite:///tmp/review-memory.db\"\nconnect_timeout_secs = 17\n"
                    .to_string(),
                Reading::V1,
            ),
            (
                "r4: a V3 TTS alias beside a V2 TTS block",
                "[providers.tts.openai.work]\napi_key = \"KEEP-TTS-KEY\"\nvoice = \"nova\"\n\n\
                 [tts.openai]\nvoice = \"alloy\"\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "r4: a V3 transcription alias beside the shared transcription table",
                "[providers.transcription.openai.work]\nmodel = \"whisper-1\"\n\n\
                 [transcription.openai]\napi_key = \"KEEP-STT-KEY\"\nmodel = \"keep-shared-model\"\n"
                    .to_string(),
                Reading::V3,
            ),
            (
                "r4: a channel alias under the channels_config spelling",
                "[channels_config.discord.work]\nenabled = true\nbot_token = \"KEEP-DISCORD-TOKEN\"\n"
                    .to_string(),
                Reading::V3,
            ),
            (
                "r4: local Whisper settings in the shared transcription table",
                "[transcription.local_whisper]\nurl = \"http://127.0.0.1:8001/v1/transcribe\"\n\
                 bearer_token = \"KEEP-LOCAL-TOKEN\"\ntimeout_secs = 17\n"
                    .to_string(),
                Reading::V1,
            ),
            (
                "r4: a legacy provider synonym with its header map",
                "[providers.models.grok]\nmodel = \"grok-4\"\n\n\
                 [providers.models.grok.extra_headers]\nX-Trace = \"keep-header\"\n"
                    .to_string(),
                Reading::V1,
            ),
            (
                "r5: a V2 Feishu channel under channels_config beside a provider alias",
                "[providers.models.ollama.work]\nmodel = \"llama3\"\n\n\
                 [channels_config.feishu]\nenabled = true\napp_id = \"keep-app\"\n\
                 app_secret = \"KEEP-FEISHU-SECRET\"\nmention_only = true\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "r5: V2 Discord history under channels_config beside a provider alias",
                "[providers.models.ollama.work]\nmodel = \"llama3\"\n\n\
                 [channels_config.discord_history]\nenabled = true\n\
                 bot_token = \"KEEP-HISTORY-TOKEN\"\n"
                    .to_string(),
                Reading::Refused,
            ),
            (
                "control: a V2 profile with its header map",
                "[providers.models.openai]\nmodel = \"gpt-4o\"\n\n\
                 [providers.models.openai.extra_headers]\nX-Trace = \"keep\"\n"
                    .to_string(),
                Reading::V1,
            ),
            (
                "control: the V3 file the inference exists for",
                HAND_WRITTEN_V3_WITHOUT_VERSION.to_string(),
                Reading::V3,
            ),
        ];
        for (case, raw, required) in cases {
            let value: toml::Value = toml::from_str(&raw).unwrap();
            let reading = match detect_version(&value) {
                Ok(1) => Reading::V1,
                Ok(INFERRED_SCHEMA_VERSION) => Reading::V3,
                Ok(other) => panic!("{case}: read as V{other}"),
                Err(_) => Reading::Refused,
            };
            assert_eq!(reading, required, "{case}");
            if required == Reading::Refused {
                assert!(migrate_file_with_notices(&raw).is_err(), "{case}");
                continue;
            }
            migrate_to_current(&raw).unwrap_or_else(|err| panic!("{case}: {err:#}"));
            let resilient = migrate_to_current_salvaged(&raw);
            assert!(
                resilient.dropped.is_empty() && resilient.dropped_security.is_empty(),
                "{case}: {:?} {:?}",
                resilient.dropped,
                resilient.dropped_security
            );
        }
    }

    /// The top-level sections of the current layout, each as a file of its
    /// own: every section of the bundled V4 fixture, and every section the
    /// default config writes.
    fn current_layout_sections() -> Vec<(String, toml::Table)> {
        let fixture: toml::Table = toml::from_str(include_str!("../fixtures/v4.toml")).unwrap();
        let defaults = match toml::Value::try_from(Config::default()).unwrap() {
            toml::Value::Table(table) => table,
            other => panic!("the default config is not a table: {other}"),
        };
        let mut sections: Vec<(String, toml::Table)> =
            [("fixture", fixture), ("default", defaults)]
                .into_iter()
                .flat_map(|(source, table)| {
                    table
                        .into_iter()
                        .filter(|(key, _)| key != "schema_version" && key != "tools")
                        .map(move |(key, section)| {
                            (
                                format!("{source}: {key}"),
                                toml::Table::from_iter([(key, section)]),
                            )
                        })
                })
                .collect();
        // Every property the schema exposes, set, in every keyed section.
        let every = match toml::Value::try_from(config_with_every_property_set()).unwrap() {
            toml::Value::Table(table) => table,
            other => panic!("the config is not a table: {other}"),
        };
        sections.extend(
            every
                .into_iter()
                .filter(|(key, _)| key != "schema_version" && key != "tools")
                .map(|(key, section)| {
                    (
                        format!("every property: {key}"),
                        toml::Table::from_iter([(key, section)]),
                    )
                }),
        );
        // The same sections under every other spelling the schema accepts.
        for (alias, spelled) in top_level_aliases() {
            let respelled: Vec<(String, toml::Table)> = sections
                .iter()
                .filter_map(|(label, section)| {
                    section.get(&spelled).map(|content| {
                        (
                            format!("{label}, spelled `{alias}`"),
                            toml::Table::from_iter([(alias.to_string(), content.clone())]),
                        )
                    })
                })
                .collect();
            sections.extend(respelled);
        }
        sections
    }

    /// Every property the schema exposes on `config` set to a value other
    /// than its current one, where the setter takes it. References to other
    /// aliases and nested lists are left as they are.
    fn set_every_property(config: &mut Config) {
        use crate::traits::PropKind;
        for field in config.prop_fields() {
            let current = field.display_value.clone();
            let value = match field.kind {
                PropKind::Bool => (current != "true").to_string(),
                PropKind::Integer => current
                    .parse::<i64>()
                    .map_or_else(|_| "7".to_string(), |n| (n + 1).to_string()),
                PropKind::Float => current
                    .parse::<f64>()
                    .map_or_else(|_| "1.5".to_string(), |x| (x + 0.5).to_string()),
                // A provider reference is current content only in the dotted
                // `<family>.<alias>` form; a bare one is the form the older
                // layouts' migration completes.
                PropKind::String if field.name.ends_with("provider") => "probe.default".to_string(),
                PropKind::String if current.is_empty() => "probe".to_string(),
                PropKind::String => format!("{current}-probe"),
                PropKind::Enum => {
                    let Some(other) = field
                        .enum_variants
                        .and_then(|variants| variants().into_iter().find(|v| *v != current))
                    else {
                        continue;
                    };
                    other
                }
                PropKind::StringArray => "probe-a,probe-b".to_string(),
                _ => continue,
            };
            let _ = config.set_prop(&field.name, &value);
        }
    }

    /// A config with an alias, `probe`, in every keyed section the schema
    /// exposes, each optional table the parent types accept materialized,
    /// and every property set: every current path, rather than the
    /// fixture's and the defaults' selection.
    fn config_with_every_property_set() -> Config {
        let mut config = Config::default();
        // Keyed sections nest (`providers.models.<family>`), so repeat until
        // nothing new is created.
        for _ in 0..4 {
            let mut created = false;
            for section in Config::map_key_sections() {
                if config.create_map_key(section.path, "probe") == Ok(true) {
                    created = true;
                }
            }
            if !created {
                break;
            }
        }
        set_every_property(&mut config);
        // An optional table appears in neither the properties nor the
        // serialized config while unset: give each one a parent type accepts
        // an empty table, where its defaults read, then set its properties.
        let mut value = toml::Value::try_from(&config).unwrap();
        for parent in [
            "security",
            "reliability",
            "memory",
            "storage",
            "cost",
            "tts",
            "transcription",
            "tunnel",
            "web_search",
            "channels",
            "gateway.pairing_dashboard",
        ] {
            for key in accepted_at(parent) {
                let mut candidate = value.clone();
                let Some(section) = parent
                    .split('.')
                    .try_fold(&mut candidate, |value, key| value.get_mut(key))
                    .and_then(toml::Value::as_table_mut)
                else {
                    continue;
                };
                if section.contains_key(*key) {
                    continue;
                }
                section.insert((*key).to_string(), toml::Value::Table(toml::Table::new()));
                if let Ok(parsed) = candidate.try_into::<Config>() {
                    let written = toml::Value::try_from(&parsed).unwrap();
                    if value_at(&written, &format!("{parent}.{key}")).is_some() {
                        value = written;
                    }
                }
            }
        }
        let mut config: Config = value.try_into().unwrap();
        set_every_property(&mut config);
        config
    }

    /// The keys `T` accepts, its serde aliases included: the field list
    /// serde's derive hands to `deserialize_struct`, which names each alias
    /// beside its field.
    fn accepted_keys<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
        #[derive(Debug)]
        struct Fields(&'static [&'static str]);
        impl std::fmt::Display for Fields {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{:?}", self.0)
            }
        }
        impl std::error::Error for Fields {}
        impl serde::de::Error for Fields {
            fn custom<M: std::fmt::Display>(_: M) -> Self {
                Fields(&[])
            }
        }
        struct Recorder;
        impl<'de> serde::Deserializer<'de> for Recorder {
            type Error = Fields;
            fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Fields> {
                Err(Fields(&[]))
            }
            fn deserialize_struct<V: serde::de::Visitor<'de>>(
                self,
                _: &'static str,
                fields: &'static [&'static str],
                _: V,
            ) -> Result<V::Value, Fields> {
                Err(Fields(fields))
            }
            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
                bytes byte_buf option unit unit_struct newtype_struct seq tuple
                tuple_struct map enum identifier ignored_any
            }
        }
        let fields = match T::deserialize(Recorder) {
            Err(Fields(fields)) => fields,
            Ok(_) => &[],
        };
        assert!(
            !fields.is_empty(),
            "`{}` lists no fields to serde",
            std::any::type_name::<T>()
        );
        fields
    }

    /// Each top-level key `Config` accepts that names no section, with the
    /// section it spells: a file holding both is refused by serde as a
    /// duplicate field.
    fn top_level_aliases() -> Vec<(&'static str, String)> {
        let canonical = current_top_level_sections();
        let defaults = toml::Value::try_from(Config::default()).unwrap();
        let fixture: toml::Value = toml::from_str(include_str!("../fixtures/v4.toml")).unwrap();
        accepted_keys::<Config>()
            .iter()
            .copied()
            .filter(|key| !canonical.contains(*key))
            .map(|alias| {
                let spelled = canonical
                    .iter()
                    .find(|name| {
                        let content = fixture
                            .get(name.as_str())
                            .or_else(|| defaults.get(name.as_str()))
                            .cloned()
                            .unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
                        let both = toml::Value::Table(toml::Table::from_iter([
                            ((*name).clone(), content.clone()),
                            (alias.to_string(), content),
                        ]));
                        both.try_into::<Config>()
                            .is_err_and(|err| err.to_string().contains("duplicate field"))
                    })
                    .unwrap_or_else(|| {
                        panic!("`{alias}` is accepted by `Config` but spells no section")
                    });
                (alias, spelled.clone())
            })
            .collect()
    }

    /// `section` with the V3 layout probe beside it.
    fn beside_the_v3_layout(section: &toml::Table) -> toml::Value {
        let mut file = toml::Value::Table(section.clone());
        let probe: toml::Value = toml::from_str(V3_LAYOUT_PROBE).unwrap();
        let cron = file
            .as_table_mut()
            .unwrap()
            .entry("cron")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        cron.as_table_mut()
            .unwrap()
            .extend(probe["cron"].as_table().unwrap().clone());
        file
    }

    /// Nothing in the current layout counts as an older layout: a file
    /// holding any current section beside a V3 one is read as V3, never
    /// refused as mixed. Alone, no current section is refused.
    #[test]
    fn current_sections_never_read_as_an_older_layout() {
        for (label, section) in current_layout_sections() {
            assert_eq!(
                detect_version(&beside_the_v3_layout(&section)).ok(),
                Some(INFERRED_SCHEMA_VERSION),
                "{label}: read as an older layout"
            );
            assert!(
                detect_version(&toml::Value::Table(section.clone())).is_ok(),
                "{label}: refused on its own"
            );
        }
        let mut fixture: toml::Table = toml::from_str(include_str!("../fixtures/v4.toml")).unwrap();
        fixture.remove("schema_version");
        assert_eq!(
            detect_version(&toml::Value::Table(fixture)).ok(),
            Some(INFERRED_SCHEMA_VERSION),
            "the whole V4 fixture, unversioned"
        );
    }

    /// Every leaf path under `value`, arrays counted as leaves.
    fn leaf_paths(value: &toml::Value, prefix: &str, out: &mut Vec<String>) {
        match value {
            toml::Value::Table(table) => {
                for (key, child) in table {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    leaf_paths(child, &path, out);
                }
            }
            _ => out.push(prefix.to_string()),
        }
    }

    /// `file` stated as schema `version` and loaded: the migrated value, and
    /// the typed config read from it.
    fn stated_reading(file: &toml::Table, version: u32) -> (toml::Value, toml::Value) {
        let mut stated = file.clone();
        stated.insert(
            "schema_version".to_string(),
            toml::Value::Integer(i64::from(version)),
        );
        let raw = toml::to_string(&stated).unwrap();
        let (migrated, _) =
            migrate_value(&raw).unwrap_or_else(|err| panic!("V{version}: {err:#}\n{raw}"));
        let config: Config = migrated
            .clone()
            .try_into()
            .unwrap_or_else(|err| panic!("V{version}: {err:#}\n{raw}"));
        (migrated, toml::Value::try_from(config).unwrap())
    }

    /// The paths at which reading `file` as V1 and as V3 configures something
    /// different. Only paths either migrated file holds are compared, so
    /// defaults derived from the environment (`HOME`) do not count.
    fn readings_differ(file: &toml::Table) -> Vec<String> {
        let (as_v1, typed_v1) = stated_reading(file, 1);
        let (as_v3, typed_v3) = stated_reading(file, 3);
        let mut paths = Vec::new();
        leaf_paths(&as_v1, "", &mut paths);
        leaf_paths(&as_v3, "", &mut paths);
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .filter(|path| value_at(&typed_v1, path) != value_at(&typed_v3, path))
            .collect()
    }

    /// The sections only the V3 layout has, which a file read as V3 holds:
    /// alias-keyed providers of every category and channels, cron jobs, the
    /// profiles and peer groups, the alias-keyed storage backends, the
    /// renamed tunnel and search providers, and agents.
    const V3_ONLY_SECTIONS: &[&[&str]] = &[
        &["providers", "models"],
        &["providers", "tts"],
        &["providers", "transcription"],
        &["cron"],
        &["risk_profiles"],
        &["runtime_profiles"],
        &["peer_groups"],
        &["storage", "sqlite"],
        &["storage", "postgres"],
        &["storage", "qdrant"],
        &["storage", "markdown"],
        &["storage", "lucid"],
        &["tunnel", "tunnel_provider"],
        &["web_search", "search_provider"],
        &["agents"],
    ];

    /// A file with no section that tells the layouts apart is read as V1,
    /// and that reading configures exactly what the V3 reading would. Checked
    /// for each current section under each spelling, and for the whole V4
    /// fixture without its V3-only sections.
    #[test]
    fn a_file_with_no_layout_evidence_reads_the_same_as_v1_and_as_v3() {
        for (label, section) in current_layout_sections() {
            if detect_version(&toml::Value::Table(section.clone())).ok() != Some(1) {
                continue;
            }
            let differ = readings_differ(&section);
            assert!(
                differ.is_empty(),
                "{label}: the V1 reading differs at {differ:?}"
            );
        }

        let mut fixture: toml::Table = toml::from_str(include_str!("../fixtures/v4.toml")).unwrap();
        for key in ["schema_version", "tools"] {
            fixture.remove(key);
        }
        for path in V3_ONLY_SECTIONS {
            let (last, parents) = path.split_last().unwrap();
            let parent = parents.iter().try_fold(&mut fixture, |table, key| {
                table.get_mut(*key).and_then(toml::Value::as_table_mut)
            });
            if let Some(parent) = parent {
                parent.remove(*last);
            }
        }
        if let Some(toml::Value::Table(channels)) = fixture.get_mut("channels") {
            for kind in crate::schema::v2::V3_CHANNEL_TYPES {
                channels.remove(*kind);
            }
        }
        assert_eq!(
            detect_version(&toml::Value::Table(fixture.clone())).ok(),
            Some(1),
            "the fixture without its V3-only sections"
        );
        let differ = readings_differ(&fixture);
        assert!(differ.is_empty(), "the V1 reading differs at {differ:?}");
    }

    /// The current `[transcription]` still reads V2's Groq settings and
    /// per-family STT tables, spelled as V2 spelled them, so the V1 reading
    /// leaves them where they are, with every value: no move to a
    /// `[providers.transcription]` alias whose fields differ (`uri` for
    /// `url`, no endpoint for Groq).
    #[test]
    fn the_v1_reading_keeps_the_shared_transcription_settings_in_place() {
        let file: toml::Table = toml::from_str(
            "[transcription]\napi_url = \"https://stt.example.invalid/v1\"\n\
             model = \"whisper-keep\"\n\n\
             [transcription.openai]\nmodel = \"whisper-1\"\n\n\
             [transcription.google]\nlanguage_code = \"de-DE\"\n\n\
             [transcription.local_whisper]\nurl = \"http://127.0.0.1:8001/v1/transcribe\"\n\
             bearer_token = \"KEEP-LOCAL-TOKEN\"\ntimeout_secs = 17\n",
        )
        .unwrap();
        assert_eq!(
            detect_version(&toml::Value::Table(file.clone())).ok(),
            Some(1)
        );
        let (as_v1, typed_v1) = stated_reading(&file, 1);
        for (path, kept) in [
            ("transcription.api_url", "https://stt.example.invalid/v1"),
            ("transcription.model", "whisper-keep"),
            ("transcription.openai.model", "whisper-1"),
            ("transcription.google.language_code", "de-DE"),
            (
                "transcription.local_whisper.url",
                "http://127.0.0.1:8001/v1/transcribe",
            ),
            (
                "transcription.local_whisper.bearer_token",
                "KEEP-LOCAL-TOKEN",
            ),
        ] {
            assert_eq!(
                value_at(&typed_v1, path).and_then(toml::Value::as_str),
                Some(kept),
                "{path}"
            );
        }
        assert_eq!(
            value_at(&typed_v1, "transcription.local_whisper.timeout_secs")
                .and_then(toml::Value::as_integer),
            Some(17)
        );
        assert!(
            value_at(&as_v1, "providers.transcription").is_none(),
            "nothing moves under `[providers.transcription]`: {as_v1:#?}"
        );
        assert!(readings_differ(&file).is_empty());
    }

    // ── Every spelling the types accept, every destination ──────

    /// The keys the current schema accepts at `parent`, a dotted path whose
    /// `*` stands for any alias.
    fn accepted_at(parent: &str) -> &'static [&'static str] {
        use crate::schema::{
            AliasedAgentConfig, ChannelsConfig, CostConfig, EmbeddingRouteConfig, MemoryConfig,
            ModelRouteConfig, PairingDashboardConfig, ReliabilityConfig, SecurityConfig,
            StorageConfig, TranscriptionConfig, TtsConfig, TunnelConfig, WebSearchConfig,
        };
        match parent {
            "" => accepted_keys::<Config>(),
            "providers" => accepted_keys::<crate::providers::Providers>(),
            "security" => accepted_keys::<SecurityConfig>(),
            "reliability" => accepted_keys::<ReliabilityConfig>(),
            "memory" => accepted_keys::<MemoryConfig>(),
            "storage" => accepted_keys::<StorageConfig>(),
            "cost" => accepted_keys::<CostConfig>(),
            "tts" => accepted_keys::<TtsConfig>(),
            "transcription" => accepted_keys::<TranscriptionConfig>(),
            "gateway.pairing_dashboard" => accepted_keys::<PairingDashboardConfig>(),
            "tunnel" => accepted_keys::<TunnelConfig>(),
            "web_search" => accepted_keys::<WebSearchConfig>(),
            "channels" => accepted_keys::<ChannelsConfig>(),
            "agents.*" => accepted_keys::<AliasedAgentConfig>(),
            "model_routes" => accepted_keys::<ModelRouteConfig>(),
            "embedding_routes" => accepted_keys::<EmbeddingRouteConfig>(),
            other => panic!("no type is mapped for `{other}`; add it to `accepted_at`"),
        }
    }

    /// No older-layout setting is a spelling the current schema reads where
    /// it is: its key is neither a field nor a serde alias of the type at its
    /// parent. The accepted keys come from the types, aliases included, so a
    /// spelling such as `channels_config`, an alias of `channels`, cannot be
    /// taken for an older layout.
    #[test]
    fn no_legacy_layout_path_is_a_current_spelling() {
        // The mapping names the right types: each accepts every key the
        // default config or the fixture writes there.
        let defaults = toml::Value::try_from(Config::default()).unwrap();
        let fixture: toml::Value = toml::from_str(include_str!("../fixtures/v4.toml")).unwrap();
        for parent in [
            "",
            "providers",
            "security",
            "reliability",
            "memory",
            "storage",
            "cost",
            "tts",
            "transcription",
            "gateway.pairing_dashboard",
            "tunnel",
            "web_search",
            "channels",
        ] {
            let written = if parent.is_empty() {
                defaults.as_table()
            } else {
                value_at(&defaults, parent)
                    .or_else(|| value_at(&fixture, parent))
                    .and_then(toml::Value::as_table)
            }
            .unwrap_or_else(|| panic!("nothing is written at `{parent}`"));
            for key in written.keys() {
                assert!(
                    accepted_at(parent).contains(&key.as_str()),
                    "`{parent}` is mapped to a type that does not read `{key}`"
                );
            }
        }
        for agent in fixture["agents"].as_table().unwrap().values() {
            for key in agent.as_table().unwrap().keys() {
                assert!(accepted_at("agents.*").contains(&key.as_str()), "{key}");
            }
        }

        for pattern in LEGACY_LAYOUT_PATHS {
            let (key, parent) = pattern.split_last().unwrap();
            let parent = parent.join(".");
            assert!(
                !accepted_at(&parent).contains(key),
                "`{}` is a spelling the current schema reads",
                pattern.join(".")
            );
        }
        for key in V2_ONLY_AGENT_KEYS {
            assert!(!accepted_at("agents.*").contains(key), "agents.*.{key}");
        }
        for routes in ["model_routes", "embedding_routes"] {
            assert!(
                !accepted_at(routes).contains(&"provider"),
                "{routes} entries read `provider`"
            );
        }
    }

    /// The top-level spellings the current schema accepts that an older
    /// typed lens (`V1Config`, `V2Config`) also reads, so the chain reshapes
    /// them, and how the detector tells the layouts apart there. Generated
    /// from the types: a new shared spelling fails until it is listed here
    /// and handled.
    #[test]
    fn spellings_shared_with_older_layouts_are_told_apart() {
        const TOLD_APART: &[(&str, &str)] = &[
            (
                "agents",
                "V2-only agent keys are older; current agent keys are V3",
            ),
            (
                "channels",
                "a channel type holding fields directly is older; alias-keyed is V3",
            ),
            (
                "channels_config",
                "an alias of `channels`, read the same way",
            ),
            ("cost", "`cost.prices` is older"),
            (
                "cron",
                "`[[cron.jobs]]` and the subsystem knobs are older; job tables are V3",
            ),
            ("embedding_routes", "a route naming `provider` is older"),
            ("model_routes", "a route naming `provider` is older"),
            (
                "providers",
                "a family holding fields directly, or a value on `[providers]`, is older; \
                 alias-keyed families of every category are V3",
            ),
            ("schema_version", "it states the version"),
        ];
        let read_by_an_older_lens = |key: &str| {
            let file = toml::Value::Table(toml::Table::from_iter([(
                key.to_string(),
                toml::Value::String("x".into()),
            )]));
            let v1 = match file.clone().try_into::<V1Config>() {
                Ok(v1) => !v1.passthrough.contains_key(key),
                Err(_) => true,
            };
            let v2 = match file.try_into::<V2Config>() {
                Ok(v2) => !v2.passthrough.contains_key(key),
                Err(_) => true,
            };
            v1 || v2
        };
        let mut shared: Vec<&str> = accepted_keys::<Config>()
            .iter()
            .copied()
            .filter(|key| read_by_an_older_lens(key))
            .collect();
        shared.sort_unstable();
        let listed: Vec<&str> = TOLD_APART.iter().map(|(key, _)| *key).collect();
        assert_eq!(shared, listed);
    }

    /// `V3_AGENT_FIELDS` names exactly the keys a current agent accepts.
    #[test]
    fn v3_agent_fields_are_the_current_agent_fields() {
        let mut accepted = accepted_keys::<crate::schema::AliasedAgentConfig>().to_vec();
        accepted.sort_unstable();
        assert_eq!(accepted, V3_AGENT_FIELDS);
    }

    /// Each V3-only destination is a spelling the current schema reads there.
    #[test]
    fn v3_only_destinations_are_current_spellings() {
        for pattern in V3_ONLY_DESTINATIONS {
            let named: Vec<&str> = pattern
                .iter()
                .copied()
                .filter(|key| *key != ANY_KEY)
                .collect();
            let (key, parent) = named.split_last().unwrap();
            assert!(
                accepted_at(&parent.join(".")).contains(key),
                "`{}` is not a current spelling",
                pattern.join(".")
            );
        }
    }

    /// Each provider category's aliases are V3 evidence on their own, and
    /// beside an older setting the file is refused.
    #[test]
    fn v4_review_r4_every_provider_category_is_v3_evidence() {
        for alias in [
            "[providers.models.ollama.work]\nmodel = \"llama3\"\n",
            "[providers.tts.openai.work]\nvoice = \"nova\"\n",
            "[providers.transcription.openai.work]\nmodel = \"whisper-1\"\n",
        ] {
            assert_eq!(
                detect_version(&toml::from_str(alias).unwrap()).ok(),
                Some(INFERRED_SCHEMA_VERSION),
                "{alias}"
            );
            let mixed = format!("default_model = \"legacy\"\n{alias}");
            assert!(
                detect_version(&toml::from_str(&mixed).unwrap()).is_err(),
                "{mixed}"
            );
        }
    }

    /// Destinations the chain writes into that V2 had as well, or whose only
    /// older source is retired under either reading, so holding one beside
    /// that source tells nothing about the layout.
    const DESTINATIONS_SHARED_WITH_V2: &[&str] =
        &["schema_version", "scheduler", "gateway.pairing_code"];

    /// `value` with every leaf changed: a value no migration would write.
    fn changed(value: &toml::Value) -> toml::Value {
        match value {
            toml::Value::String(text) => toml::Value::String(format!("{text}-v3")),
            toml::Value::Integer(n) => toml::Value::Integer(n + 1),
            toml::Value::Float(x) => toml::Value::Float(x + 0.5),
            toml::Value::Boolean(b) => toml::Value::Boolean(!b),
            toml::Value::Array(items) if items.is_empty() => {
                toml::Value::Array(vec![toml::Value::String("v3".into())])
            }
            toml::Value::Array(items) => toml::Value::Array(items.iter().map(changed).collect()),
            toml::Value::Table(table) => toml::Value::Table(
                table
                    .iter()
                    .map(|(key, value)| (key.clone(), changed(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// `root` with `value` set at a dotted path, creating tables on the way.
    fn set_at(root: &mut toml::Value, dotted: &str, value: toml::Value) {
        let mut keys: Vec<&str> = dotted.split('.').collect();
        let last = keys.pop().unwrap();
        let mut table = root.as_table_mut().unwrap();
        for key in keys {
            table = table
                .entry(key)
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .unwrap_or_else(|| panic!("`{dotted}` crosses a value"));
        }
        table.insert(last.to_string(), value);
    }

    /// An older setting beside a V3 value at a place its own migration
    /// writes is refused: read as V1, the migration would merge the two and
    /// keep only one. Generated from the chain: every leaf it writes for each
    /// older-layout example is tried.
    #[test]
    fn a_legacy_setting_beside_what_its_migration_writes_is_refused() {
        for (path, raw) in &legacy_layout_examples() {
            let (path, raw) = (path.as_str(), raw.as_str());
            let legacy: toml::Value = toml::from_str(raw).unwrap();
            let migrated = run_chain(legacy.clone(), 1, &mut Vec::new())
                .unwrap_or_else(|err| panic!("{path}: {err:#}"));
            let mut written = Vec::new();
            leaf_paths(&migrated, "", &mut written);
            for leaf in written {
                if value_at(&legacy, &leaf).is_some()
                    || DESTINATIONS_SHARED_WITH_V2
                        .iter()
                        .any(|shared| leaf == *shared || leaf.starts_with(&format!("{shared}.")))
                {
                    continue;
                }
                let mut file = legacy.clone();
                set_at(
                    &mut file,
                    &leaf,
                    changed(value_at(&migrated, &leaf).unwrap()),
                );
                assert!(
                    detect_version(&file).is_err(),
                    "{path} beside a V3 value at `{leaf}`, which its migration writes, \
                     must be refused as mixed:\n{}",
                    toml::to_string(&file).unwrap()
                );
            }
        }
    }

    /// One file per key retired at V4, which a V3 file may still hold.
    const KEYS_RETIRED_AT_V4: &[(&str, &str)] = &[
        (
            "security.nevis",
            "[security.nevis]\nclient_secret = \"x\"\n",
        ),
        (
            "agents.x.compact_context",
            "[agents.x]\ncompact_context = true\n",
        ),
        (
            "agents.x.max_tool_iterations",
            "[agents.x]\nmax_tool_iterations = 40\n",
        ),
        (
            "agents.x.max_history_messages",
            "[agents.x]\nmax_history_messages = 10\n",
        ),
        (
            "agents.x.max_context_tokens",
            "[agents.x]\nmax_context_tokens = 1000\n",
        ),
        (
            "agents.x.memory_recall_limit",
            "[agents.x]\nmemory_recall_limit = 5\n",
        ),
        (
            "agents.x.parallel_tools",
            "[agents.x]\nparallel_tools = true\n",
        ),
        (
            "agents.x.tool_dispatcher",
            "[agents.x]\ntool_dispatcher = \"native\"\n",
        ),
        (
            "agents.x.strict_tool_parsing",
            "[agents.x]\nstrict_tool_parsing = true\n",
        ),
        (
            "runtime_profiles.x.context_compression.summary_model",
            "[runtime_profiles.x.context_compression]\nsummary_model = \"haiku\"\n",
        ),
        ("channels.wati", "[channels.wati]\napi_token = \"x\"\n"),
        (
            "channels_config.wati",
            "[channels_config.wati]\napi_token = \"x\"\n",
        ),
        ("twitter", "[twitter]\nbearer_token = \"x\"\n"),
        ("reddit", "[reddit]\nclient_id = \"x\"\n"),
        ("channels.notion", "[channels.notion]\ntoken = \"x\"\n"),
    ];

    /// A key retired at V4, in a file with nothing else to decide by, means
    /// the same under either reading, or sits on an agent holding no current
    /// agent setting. Such an agent is read as the V2 agent it can only be,
    /// as before V4: the V1 reading binds it to profiles and reads its
    /// `max_tool_iterations` as its V2 iteration limit, and changes nothing
    /// outside the agent and its profiles. Generated from `RETIRED_KEYS`.
    #[test]
    fn keys_retired_at_v4_read_the_same_or_as_a_v2_agent() {
        for key in RETIRED_KEYS.iter().filter(|key| key.retired_in >= 4) {
            let path = key.path.join(".").replace(ANY_KEY, "x");
            assert!(
                KEYS_RETIRED_AT_V4.iter().any(|(sample, _)| *sample == path),
                "`{path}` has no sample"
            );
        }
        for (path, raw) in KEYS_RETIRED_AT_V4 {
            let file: toml::Table = toml::from_str(raw).unwrap();
            match detect_version(&toml::Value::Table(file.clone())) {
                // Beside a V3 setting it is retired with a notice.
                Ok(INFERRED_SCHEMA_VERSION) => {}
                Ok(1) => {
                    let differ = readings_differ(&file);
                    if path.starts_with("agents.") {
                        assert!(
                            differ.iter().all(|at| [
                                "agents.x",
                                "risk_profiles",
                                "runtime_profiles"
                            ]
                            .iter()
                            .any(|within| at.starts_with(within))),
                            "{path}: read as V1 it configures more than its agent, at {differ:?}"
                        );
                    } else {
                        assert!(
                            differ.is_empty(),
                            "{path}: read as V1 it configures something else, at {differ:?}"
                        );
                    }
                }
                Ok(other) => panic!("{path}: read as V{other}"),
                Err(err) => panic!("{path} is refused: {err}"),
            }
        }
    }

    /// The content check in `holds_a_v2_field` compares a child with what
    /// the entry writes back, which is exact while no nested field of a
    /// provider or channel entry leaves out a sub-field at its default. The
    /// default-skipping fields of the entry types are all scalars on the
    /// entry itself, and the nested children are maps (`extra_headers`,
    /// `pricing`, `chat_template_kwargs`, `provider_extra`, the git channel's
    /// `events`, whose routes write every field they hold), so a V2 child
    /// holding a default value still reads as that child.
    #[test]
    fn a_v2_child_holding_default_values_is_still_a_field() {
        for raw in [
            "[channels.git]\nrepos = [\"zeroclaw-labs/zeroclaw\"]\n\n\
             [channels.git.events.push]\nmessage = false\n",
            "[providers.models.openai]\nmodel = \"gpt-4o\"\n\n\
             [providers.models.openai.pricing]\ninput = 0.0\n",
        ] {
            assert_eq!(
                detect_version(&toml::from_str(raw).unwrap()).ok(),
                Some(1),
                "{raw}"
            );
        }
        // A key no route has is not a field of the entry: an alias beside
        // its fields.
        let raw = "[channels.git]\nrepos = [\"zeroclaw-labs/zeroclaw\"]\n\n\
                   [channels.git.events.push]\nbogus = 1\n";
        assert!(
            detect_version(&toml::from_str(raw).unwrap()).is_err(),
            "{raw}"
        );
    }

    /// When the older part of a mixed file is a key the current layout
    /// retires anyway, adding the version is the whole fix, so the refusal
    /// says that first; otherwise it does not claim it.
    #[test]
    fn a_mixed_file_whose_older_part_is_retired_says_the_one_line_fix() {
        for retired in [
            "[identity]\nformat = \"openclaw\"\n",
            "[swarms.old]\nstrategy = \"sequential\"\n",
            "[reliability]\nfallback_providers = [\"openai\"]\n",
        ] {
            let raw = format!("{HAND_WRITTEN_V3_WITHOUT_VERSION}\n{retired}");
            let err = detect_version(&toml::from_str(&raw).unwrap())
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("is retired (")
                    && err.contains(
                        "so if the rest of the file is current, adding `schema_version = 4` \
                         as its first line is the whole fix"
                    ),
                "{err}"
            );
        }
        // It says where the setting lives now.
        let raw = format!("{HAND_WRITTEN_V3_WITHOUT_VERSION}\n[identity]\nformat = \"openclaw\"\n");
        let err = detect_version(&toml::from_str(&raw).unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("move it to `[agents.<alias>.identity]`"),
            "{err}"
        );
        let raw = format!("default_model = \"legacy\"\n{HAND_WRITTEN_V3_WITHOUT_VERSION}");
        let err = detect_version(&toml::from_str(&raw).unwrap())
            .unwrap_err()
            .to_string();
        assert!(!err.contains("is retired"), "{err}");
    }

    /// Both retirement appliers retire a key under every spelling the
    /// schema accepts for its parents, with the same notices, and the
    /// lookup used for environment overrides covers those spellings too.
    #[test]
    fn both_retirement_appliers_retire_under_every_spelling() {
        let raw = "schema_version = 4\n\n[channels_config.notion.main]\ntoken = \"x\"\n\n\
                   [channels_config.wati.old]\napi_token = \"y\"\n";
        let mut value: toml::Value = toml::from_str(raw).unwrap();
        let mut value_notices = Vec::new();
        for version in 2..=CURRENT_SCHEMA_VERSION {
            apply_retired_keys(&mut value, version, RETIRED_KEYS, &mut value_notices);
        }
        let mut doc: toml_edit::DocumentMut = raw.parse().unwrap();
        let doc_notices = apply_retired_keys_to_doc(doc.as_table_mut());
        assert_eq!(value_notices, doc_notices);
        let paths: Vec<String> = doc_notices
            .iter()
            .filter_map(|notice| match notice {
                MigrationNotice::Removed { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert!(
            paths.contains(&"channels_config.notion".to_string()),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"channels_config.wati".to_string()),
            "{paths:?}"
        );
        assert!(retired_key_covering(&["channels_config", "notion", "main"]).is_some());
    }

    /// Serde aliases in this crate that are not keys of the file, with why.
    const ALIASES_THAT_ARE_NOT_FILE_KEYS: &[(&str, &str)] = &[
        (
            "model_provider",
            "`V1Config`'s spelling, in the legacy catalog",
        ),
        ("model", "`V1Config`'s spelling, in the legacy catalog"),
        ("noop", "a value of `ObservabilityBackend`"),
        ("opentelemetry", "a value of `ObservabilityBackend`"),
        ("otlp", "a value of `ObservabilityBackend`"),
        ("sandbox-exec", "a value of `SandboxBackend`"),
        ("task_id", "a field of `CostRecord`, which is not config"),
    ];

    /// Every `alias = "..."` inside a `#[serde(...)]` attribute in this
    /// crate's source.
    fn serde_aliases_in_source() -> std::collections::BTreeSet<String> {
        fn visit(dir: &Path, out: &mut std::collections::BTreeSet<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, out);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let mut in_serde = false;
                for line in source.lines() {
                    let line = line.trim();
                    if line.starts_with("#[serde(") {
                        in_serde = true;
                    }
                    if in_serde {
                        let mut rest = line;
                        while let Some(at) = rest.find("alias") {
                            rest = &rest[at + "alias".len()..];
                            let Some(value) = rest.trim_start().strip_prefix('=') else {
                                continue;
                            };
                            let Some(value) = value.trim_start().strip_prefix('"') else {
                                continue;
                            };
                            if let Some(end) = value.find('"') {
                                out.insert(value[..end].to_string());
                            }
                        }
                        if line.ends_with(")]") {
                            in_serde = false;
                        }
                    }
                }
            }
        }
        let mut aliases = std::collections::BTreeSet::new();
        visit(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut aliases,
        );
        aliases
    }

    /// `ALIASED_KEYS` lists every serde alias of a file key in this crate,
    /// and each pair is real: a table holding both spellings is refused by
    /// serde as a duplicate of the written one.
    #[test]
    fn every_serde_alias_is_listed_with_its_field() {
        let listed: std::collections::BTreeSet<String> = ALIASED_KEYS
            .iter()
            .map(|(_, alias, _)| (*alias).to_string())
            .chain(
                ALIASES_THAT_ARE_NOT_FILE_KEYS
                    .iter()
                    .map(|(alias, _)| (*alias).to_string()),
            )
            .collect();
        assert_eq!(serde_aliases_in_source(), listed);

        let defaults = toml::Value::try_from(Config::default()).unwrap();
        for (parent, alias, written) in ALIASED_KEYS {
            let parent: Vec<&str> = parent
                .iter()
                .map(|key| if *key == ANY_KEY { "probe" } else { *key })
                .collect();
            // The default value where the default config writes one (an enum
            // takes only its variants), else a value of each shape.
            let default = value_at(
                &defaults,
                &parent
                    .iter()
                    .copied()
                    .chain([*written])
                    .collect::<Vec<_>>()
                    .join("."),
            )
            .cloned();
            let duplicate = default
                .into_iter()
                .chain([
                    toml::Value::Table(toml::Table::new()),
                    toml::Value::String("probe".into()),
                    toml::Value::Array(Vec::new()),
                    toml::Value::Boolean(true),
                    toml::Value::Integer(1),
                ])
                .any(|value| {
                    let mut file = toml::Value::Table(toml::Table::new());
                    for key in [*written, *alias] {
                        let path = parent
                            .iter()
                            .copied()
                            .chain([key])
                            .collect::<Vec<_>>()
                            .join(".");
                        set_at(&mut file, &path, value.clone());
                    }
                    file.try_into::<Config>().is_err_and(|err| {
                        err.to_string()
                            .contains(&format!("duplicate field `{written}`"))
                    })
                });
            assert!(
                duplicate,
                "`{alias}` is not read as `{written}` at `{}`",
                parent.join(".")
            );
        }

        // The top-level ones are exactly the ones `Config` accepts.
        let mut top: Vec<(&str, String)> = ALIASED_KEYS
            .iter()
            .filter(|(parent, _, _)| parent.is_empty())
            .map(|(_, alias, written)| (*alias, (*written).to_string()))
            .collect();
        top.sort();
        let mut generated = top_level_aliases();
        generated.sort();
        assert_eq!(top, generated);
    }

    /// A save rewrites every aliased key to its written spelling, keeping its
    /// value and comments, and refuses a table holding both spellings.
    #[test]
    fn saves_rewrite_aliased_keys_to_the_written_spelling() {
        let mut doc: toml_edit::DocumentMut = "# the channels\n\
            [channels_config.discord.work]\nbot_token = \"keep\"\n\n\
            [heartbeat]\nchannel = \"telegram\" # where to post\n\n\
            [storage.postgres.main]\ndbURL = \"postgres://keep\"\n"
            .parse()
            .unwrap();
        use_written_spellings(doc.as_table_mut()).unwrap();
        let written = doc.to_string();
        assert!(
            written.contains("# the channels\n[channels.discord.work]"),
            "{written}"
        );
        assert!(
            written.contains("target = \"telegram\" # where to post"),
            "{written}"
        );
        assert!(
            written.contains("db_url = \"postgres://keep\""),
            "{written}"
        );
        assert!(
            !written.contains("channels_config") && !written.contains("dbURL"),
            "{written}"
        );

        // A comment above a renamed key and the key's place survive too.
        let mut scalars: toml_edit::DocumentMut =
            "[heartbeat]\n# Route heartbeat to the team room.\n\
            channel = \"telegram\" # delivery channel\nmessage = \"keep\"\n"
                .parse()
                .unwrap();
        use_written_spellings(scalars.as_table_mut()).unwrap();
        let written = scalars.to_string();
        assert!(
            written.contains(
                "# Route heartbeat to the team room.\ntarget = \"telegram\" # delivery channel\n\
                 message = \"keep\""
            ),
            "{written}"
        );

        let mut both: toml_edit::DocumentMut =
            "[heartbeat]\nchannel = \"telegram\"\ntarget = \"discord\"\n"
                .parse()
                .unwrap();
        let err = use_written_spellings(both.as_table_mut())
            .unwrap_err()
            .to_string();
        assert!(err.contains("`target` and `channel`"), "{err}");
    }

    // ── V4 retirements of keys no schema field reads ────────────────

    /// A retirement must never name a key the current schema still reads:
    /// that would silently delete working configuration. Top-level removals
    /// must not be current sections, and a retired `[channels.<type>]` must not
    /// be a live channel type.
    #[test]
    fn a_retirement_never_names_a_key_the_schema_still_reads() {
        let sections = current_top_level_sections();
        for key in RETIRED_KEYS {
            if let [section] = key.path {
                assert!(
                    *section == ANY_KEY || !sections.contains(*section),
                    "`{section}` is a current top-level section and cannot be retired"
                );
            }
            if let ["channels", channel_type, ..] = key.path {
                assert!(
                    !crate::schema::v2::V3_CHANNEL_TYPES.contains(channel_type),
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

    /// A V3 file that holds the never-read `[channels.notion]` table next to
    /// live references to the `notion` channel, which the top-level `[notion]`
    /// poller registers.
    const V3_WITH_NOTION_CHANNEL: &str = r#"schema_version = 3

[notion]
enabled = true

[channels.notion.main]
token = "NOTION-SENTINEL"

# operator comment kept by the in-place migration
[channels.telegram.main]
bot_token = "telegram-token"

[agents.default]
channels = ["notion", "telegram.main"]

[escalation]
alert_channels = ["notion", "telegram.main"]

[peer_groups.notion_team]
channel = "notion"
agents = ["default"]
"#;

    /// The references to the `notion` channel that
    /// [`V3_WITH_NOTION_CHANNEL`] holds, which every path must keep.
    fn assert_notion_references_kept(root: &toml::Table) {
        let notion_and_telegram = toml::Value::Array(vec!["notion".into(), "telegram.main".into()]);
        assert_eq!(root["agents"]["default"]["channels"], notion_and_telegram);
        assert_eq!(root["escalation"]["alert_channels"], notion_and_telegram);
        assert_eq!(
            root["peer_groups"]["notion_team"]["channel"].as_str(),
            Some("notion")
        );
        assert!(
            root.contains_key("notion"),
            "the poller's own section stays"
        );
    }

    #[test]
    fn v3_to_v4_removes_the_unread_notion_table_and_keeps_poller_references() {
        // The parsed-value path (load and the typed chain).
        let value: toml::Value = toml::from_str(V3_WITH_NOTION_CHANNEL).unwrap();
        let Migrated { value, notices } = migrate_toml(value).unwrap().unwrap();
        assert_eq!(removed_paths(&notices), vec!["channels.notion"]);
        let root = value.as_table().unwrap();
        assert!(root["channels"].get("notion").is_none());
        assert!(root["channels"].get("telegram").is_some());
        assert_notion_references_kept(root);

        // The in-place document path (`config migrate` on a V3+ file).
        let (migrated, notices) = migrate_file_with_notices(V3_WITH_NOTION_CHANNEL)
            .unwrap()
            .unwrap();
        assert_eq!(removed_paths(&notices), vec!["channels.notion"]);
        assert!(!migrated.contains("NOTION-SENTINEL"), "{migrated}");
        assert!(
            migrated.contains("# operator comment kept by the in-place migration"),
            "{migrated}"
        );
        let reparsed: toml::Value = toml::from_str(&migrated).unwrap();
        assert_notion_references_kept(reparsed.as_table().unwrap());
        assert_eq!(detect_version(&reparsed).unwrap(), CURRENT_SCHEMA_VERSION);
    }

    /// A current file naming the `notion` channel in every table spelling
    /// (dotted keys and inline tables as well as headers) is left exactly as
    /// it is by the load path, `config migrate` and the save-time pass.
    #[test]
    fn notion_references_in_a_current_file_are_not_retirements() {
        let raw = format!(
            r#"schema_version = {CURRENT_SCHEMA_VERSION}
agents.default.channels = ["notion", "telegram.main"]
peer_groups = {{ notion_team = {{ channel = "notion" }} }}

[escalation]
alert_channels = ["notion"]

[tools]
optional = ["*"]
"#
        );
        let (out, notices) = apply_doc(&raw);
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(out, raw);

        let value: toml::Value = toml::from_str(&raw).unwrap();
        assert!(migrate_toml(value).unwrap().is_none());
        assert_eq!(migrate_file_with_notices(&raw).unwrap(), None);
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let report = migrate_file_in_place(&path)
            .unwrap()
            .expect("a V3 file migrates");
        let versioned = dir.path().join("config.toml.v3.backup");
        assert_eq!(
            report.version_backup_path.as_deref(),
            Some(versioned.as_path())
        );
        assert_eq!(report.from_version, 3);
        assert_eq!(std::fs::read_to_string(&versioned).unwrap(), original);
        // The lasting copy can hold retired secrets: owner-only, whatever the
        // mode of the file it copied.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&versioned).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "got {mode:o}");
        }

        // A retired key pasted back into the V4 file: the next migration is
        // retirement-only and replaces `.backup`, not the versioned copy.
        let mut current = std::fs::read_to_string(&path).unwrap();
        current.push_str("\n[security.nevis]\nenabled = true\n");
        std::fs::write(&path, &current).unwrap();
        let report = migrate_file_in_place(&path)
            .unwrap()
            .expect("the retired key is removed");
        assert_eq!(
            report.version_backup_path, None,
            "a current file keeps no copy"
        );
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

    /// A reason and its Fluent id go together: entries that share a reason
    /// share its id, and no id stands for two reasons, so the CLI can find the
    /// localized text from the English reason a notice carries.
    #[test]
    fn retirement_reasons_and_their_ids_correspond() {
        for key in RETIRED_KEYS {
            assert!(
                key.reason_id.starts_with("cli-config-retired-reason-"),
                "{key:?}"
            );
            for other in RETIRED_KEYS {
                assert_eq!(
                    key.reason == other.reason,
                    key.reason_id == other.reason_id,
                    "{:?} and {:?}",
                    key.path,
                    other.path
                );
            }
            assert_eq!(retirement_reason_id(key.reason), Some(key.reason_id));
        }
        assert_eq!(retirement_reason_id("not a retirement reason"), None);
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

    const TEST_RETIREMENTS: &[RetiredKey] = &[
        RetiredKey {
            retired_in: 9,
            path: &["old", "knob"],
            reason: "retired for the test",
            reason_id: "cli-config-retired-reason-test",
        },
        RetiredKey {
            retired_in: 8,
            path: &["other"],
            reason: "a different version",
            reason_id: "cli-config-retired-reason-test",
        },
        RetiredKey {
            retired_in: 7,
            path: &["bots", ANY_KEY, "old_limit"],
            reason: "wildcard retirement for the test",
            reason_id: "cli-config-retired-reason-test",
        },
    ];

    fn apply(raw: &str, version: u32) -> (toml::Value, Vec<MigrationNotice>) {
        let mut value: toml::Value = toml::from_str(raw).unwrap();
        let mut notices = Vec::new();
        apply_retired_keys(&mut value, version, TEST_RETIREMENTS, &mut notices);
        (value, notices)
    }

    #[test]
    fn a_retired_key_is_removed_only_at_its_version() {
        let (value, notices) = apply("other = 1\n[old]\nknob = 7\nkeep = true\n", 9);
        assert!(value["old"].get("knob").is_none());
        assert_eq!(value["old"]["keep"].as_bool(), Some(true));
        assert_eq!(
            value["other"].as_integer(),
            Some(1),
            "only version 9 entries apply"
        );
        assert_eq!(
            notices,
            vec![MigrationNotice::Removed {
                path: "old.knob".to_string(),
                reason: "retired for the test",
            }]
        );

        let (value, _) = apply("[old]\nknob = 7\n", 9);
        assert!(
            value.get("old").is_none(),
            "a literal container left empty is dropped"
        );
    }

    #[test]
    fn a_wildcard_retirement_keeps_each_alias() {
        // `z` before `a`: the file's order is not the sorted order.
        let raw =
            "[bots.z]\nold_limit = 1\n\n[bots.a]\nold_limit = 2\n\n[bots.a.limits]\nmax = 9\n";
        let (value, notices) = apply(raw, 7);
        let mut doc: toml_edit::DocumentMut = raw.parse().unwrap();
        let mut doc_notices = Vec::new();
        apply_retired_keys_to_doc_table(doc.as_table_mut(), 7, TEST_RETIREMENTS, &mut doc_notices);
        assert_eq!(
            notices, doc_notices,
            "both appliers report the same removals"
        );
        assert_eq!(notices.len(), 2);
        let from_doc: toml::Value = toml::from_str(&doc.to_string()).unwrap();
        for value in [&value, &from_doc] {
            assert!(
                value["bots"]["z"].as_table().unwrap().is_empty(),
                "an alias emptied by a retirement is kept"
            );
            assert!(value["bots"]["a"].get("old_limit").is_none());
            assert_eq!(value["bots"]["a"]["limits"]["max"].as_integer(), Some(9));
        }
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

    /// Its tool selector is explicit, so a migration has none to add.
    const UNTOUCHED_V3_BODY: &str = "locale = 'en'  # operator note: keep this explanation\n\
         \n\
         [gateway]\n\
         port = 42_617 # underscores stay\n\
         host = \"127.0.0.1\"\n\
         \n\
         # A comment block before a section.\n\
         [cost]\n\
         daily_limit_usd = 1e1\n\
         warn_at_percent = 0x50\n\
         \n\
         [tools]\n\
         optional = [\"*\"]\n";

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

    // ── R1: the dashboard's retired pairing-code length ──

    const RETIRED_CODE_LENGTH_BODY: &str = "locale = \"en\"\n\n\
         [gateway.pairing_dashboard]\n\
         code_length = 8\n\
         code_ttl_secs = 3600\n\n\
         [gateway.pairing_code]\n\
         length = 20\n\
         charset = \"unambiguous\"\n\n\
         [tools]\n\
         optional = [\"*\"]\n";

    #[test]
    fn retired_dashboard_code_length_is_removed_on_every_path() {
        for version in [3, CURRENT_SCHEMA_VERSION] {
            let raw = format!("schema_version = {version}\n{RETIRED_CODE_LENGTH_BODY}");
            let expected = ["gateway.pairing_dashboard.code_length"];

            let (migrated, notices) = migrate_file_with_notices(&raw)
                .unwrap()
                .unwrap_or_else(|| panic!("V{version}: the retired key must trigger a rewrite"));
            assert_eq!(removed_paths(&notices), expected, "V{version}");
            assert_eq!(
                migrated,
                format!(
                    "schema_version = {CURRENT_SCHEMA_VERSION}\n{}",
                    RETIRED_CODE_LENGTH_BODY.replace("code_length = 8\n", "")
                ),
                "V{version}: only the retired key and the version may change"
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
            assert!(!out.contains("code_length") && out.contains("code_ttl_secs = 3600"));
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
