use anyhow::{Context, Result};
use std::path::Path;

use crate::schema::Config;
use crate::schema::v1::V1Config;
use crate::schema::v2::V2Config;

/// The schema version this binary writes and expects on disk.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Top-level TOML keys that legacy schema versions had but V3 either
/// removed or restructured. Suppresses "unknown key" warnings on V1/V2
/// configs flowing through `migrate_to_current`: every key here is
/// consumed by `V1Config::migrate` or `V2Config::migrate`, so it's
/// expected on a stale-but-being-migrated config.
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

/// Detect a config's schema version from its parsed TOML representation.
///
/// - Missing top-level `schema_version` key → V1 (pre-versioned).
/// - Integer ≥ 1 → that integer.
/// - Anything else → error.
pub fn detect_version(value: &toml::Value) -> Result<u32> {
    let table = value
        .as_table()
        .context("config root must be a TOML table")?;
    match table.get("schema_version") {
        None => Ok(1),
        Some(toml::Value::Integer(n)) if *n >= 1 => Ok(*n as u32),
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

/// Pure migration from any supported version's TOML string into the current
/// schema version's TOML string. Returns `Ok(None)` when the input is already
/// at `CURRENT_SCHEMA_VERSION`.
///
/// Comments and decoration on keys whose dotted path survives the migration
/// are preserved via `toml_edit::DocumentMut` reconciliation (`sync_table`).
/// Keys that are renamed, removed, or restructured lose their comments — the
/// `.backup` file written by `migrate_file_in_place` retains the original
/// for manual recovery.
pub fn migrate_file(input: &str) -> Result<Option<String>> {
    let value: toml::Value = toml::from_str(input).context("failed to parse config TOML")?;
    let from = detect_version(&value)?;
    if from == CURRENT_SCHEMA_VERSION {
        return Ok(None);
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
    let migrated_value = run_chain(value, from)?;
    let migrated_table = match migrated_value {
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
        Ok(Some(doc.to_string()))
    } else {
        let serialized = toml::to_string_pretty(&toml::Value::Table(migrated_table))
            .context("failed to serialize migrated config")?;
        Ok(Some(serialized))
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
    /// [`SECRET_KEY_NAMES`].
    pub encrypt_secrets: bool,
    /// Directory containing (or to receive) the `.secret_key` used for
    /// `enc2:` encryption. Required when `encrypt_secrets` is true. The
    /// key is created with 0o600 permissions if absent — matches how the
    /// daemon's `SecretStore` behaves on first use.
    pub secret_store_dir: Option<&'a Path>,
}

/// Generate a canonical TOML config at `target_version`, derived by
/// running the V1 fixture forward through the typed migration chain.
///
/// `target_version` must be in `1..=CURRENT_SCHEMA_VERSION`. The chain is
/// the same one used to migrate real on-disk configs — V1 fixture →
/// `V1Config::migrate` → V2 typed value → `V2Config::migrate` → V3 typed
/// value — so `generate <n>` shows exactly the shape an operator running
/// `zeroclaw config migrate` would land on if they started from the V1
/// fixture.
///
/// When [`GenerateOptions::encrypt_secrets`] is set, secret-bearing
/// string values (api_key, bot_token, access_token, etc. — see
/// [`SECRET_KEY_NAMES`]) are ChaCha20-Poly1305-encrypted with the
/// `.secret_key` under `secret_store_dir`. Works at every version.
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
        run_chain_until(v1_value, 1, target_version)?
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

/// Set of TOML terminal key names whose string leaves are treated as
/// secrets by [`encrypt_secret_strings`]. Sourced from
/// `Config::secret_field_terminals()`, the macro-emitted static
/// enumeration of every `#[secret]` field reachable from the schema.
/// The set is schema-driven — adding a new `#[secret]` annotation
/// anywhere in the schema automatically extends encryption coverage
/// with no companion edit in this module.
///
/// `secret_field_terminals()` (vs. the older `prop_fields().filter(is_secret)`
/// approach) covers compound shapes like `HashMap<String, String>`
/// — `prop_fields()` intentionally skips non-Vec compound types, which
/// would silently drop e.g. `mcp.servers[*].headers` from the allowlist.
fn secret_key_names() -> &'static std::collections::HashSet<&'static str> {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static CACHE: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CACHE.get_or_init(|| Config::secret_field_terminals().into_iter().collect())
}

/// Walk a TOML tree and encrypt every string leaf whose terminal key
/// name appears in [`secret_key_names`]. Strings already in `enc2:` /
/// `enc:` form are left alone (idempotent). Arrays of strings under a
/// matching key (e.g. `paired_tokens`) are encrypted element-wise.
///
/// Works at every schema version because it operates on raw TOML
/// rather than a typed `#[secret]` index — only the *set of key names
/// to encrypt* comes from the typed schema; the walker itself doesn't
/// care about types.
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

/// Encrypt the value at this slot — a string, an array of strings, or
/// a table containing strings — using the given store. Non-string leaves
/// (numbers, bools) are left alone; the operator presumably annotated a
/// non-secret field with a secret-shaped name and we don't second-guess.
///
/// When the slot is a Table (e.g. `headers = { Authorization = "Bearer
/// ...", X-Tenant = "..." }`), every leaf in the subtree is encrypted —
/// the parent key matched the secret allowlist, so every value below it
/// inherits the secret marker. This is the contract for `HashMap<String,
/// String>`-shaped `#[secret]` fields where individual keys are
/// user-supplied and can't be checked against a static allowlist.
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
    let final_value = migrate_value(input)?;
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
pub const SECURITY_CRITICAL_KEYS: &[&str] = &["security", "risk_profiles", "peer_groups"];

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
}

/// Daemon load path with a salvage report. Degrades instead of failing:
/// strict deserialize first; else drop each invalid channel alias, channel
/// type, and top-level section (substituting `Default`); else fall back to
/// `Config::default()`. Security-critical drops log ERROR and surface in
/// `dropped_security`. `Config::validate()` is intentionally not run.
pub fn migrate_to_current_salvaged(input: &str) -> ResilientLoad {
    let value = match migrate_value(input) {
        Ok(value) => value,
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
                dropped_security: Vec::new(),
            };
        }
    };
    deserialize_resilient(value)
}

/// Parse + migrate to the current schema version as a `toml::Value`, without
/// the final typed deserialize. Shared by the strict and resilient entries.
fn migrate_value(input: &str) -> Result<toml::Value> {
    let value: toml::Value = toml::from_str(input).context("failed to parse config TOML")?;
    let from = detect_version(&value)?;
    if from == CURRENT_SCHEMA_VERSION {
        Ok(value)
    } else if from > CURRENT_SCHEMA_VERSION {
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
        )
    } else {
        run_chain(value, from)
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
        };
    }

    let mut salvaged = value;
    let mut dropped: Vec<String> = Vec::new();
    prune_bad_channel_aliases(&mut salvaged, &mut dropped);
    prune_bad_channel_types(&mut salvaged, &mut dropped);
    prune_bad_top_level_sections(&mut salvaged, &mut dropped);

    let config = salvaged.try_into::<Config>().unwrap_or_else(|err| {
        // Nothing in the root table is individually salvageable (e.g. a
        // non-table root). Boot on defaults so repair surfaces are reachable.
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
                 Run `zeroclaw config migrate` to see the precise parse error, or fix it \
                 via the gateway config editor at `/api/config`"
            )
        );
    }

    ResilientLoad {
        config,
        dropped: dropped_plain,
        dropped_security,
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

/// Drop each `[channels.<type>.<alias>]` that fails to deserialize, checked in
/// isolation so valid siblings survive. Appends `channels.<type>.<alias>`.
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

/// File-API wrapper: read disk config, migrate, write `<file>.backup`
/// adjacent to the original, then atomically replace the original. Returns
/// `Ok(None)` when already current.
///
/// Backup file is `<config_filename>.backup` (joined cross-platform via
/// `Path` ops). The write path mirrors `Config::save()` so the documented
/// durability guarantee holds end-to-end:
///
/// 1. Write the migrated content to `<path>.tmp-<uuid>` and fsync it.
/// 2. Copy the original to `<path>.backup` (existing behavior; recovery
///    rope if anything later goes wrong).
/// 3. `rename(<path>.tmp, <path>)` — atomic on Unix and on modern Windows.
/// 4. Fsync the parent directory so the rename is durable.
///
/// On rename failure the temp file is removed and the backup is restored
/// over the original so the operator never observes a partial write.
pub fn migrate_file_in_place(path: &Path) -> Result<Option<MigrateReport>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display().to_string()))?;
    let migrated = match migrate_file(&raw)? {
        Some(s) => s,
        None => return Ok(None),
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
        to_version: CURRENT_SCHEMA_VERSION,
    }))
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
}

/// Refuse to proceed if the on-disk config is at a stale schema version.
///
/// Used by CLI write commands (`config set`, `config patch`, `config init`)
/// to ensure the user explicitly opts into the migration via
/// `zeroclaw config migrate` before modifying a stale config — the alternative
/// would be a silent auto-migrate-on-write, which is harder to audit and
/// surprises users who didn't realize their config schema had changed.
///
/// - Missing file → `Ok(())` (fresh install: nothing to migrate yet).
/// - Current version → `Ok(())`.
/// - Stale (or future) version → `Err` with a message that names the disk
///   version and the command the user needs to run.
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

/// Fold a `from_key: String` value into a `to_key: Vec<String>` array on the
/// same table. Used for the singular→plural channel transforms (V1→V2:
/// `matrix.room_id` → `allowed_rooms`, `slack.channel_id` → `channel_ids`;
/// V2→V3: `discord.guild_id` → `guild_ids`, etc.).
///
/// - Removes `from_key` from the table.
/// - If the value was a non-empty string, appends it to `to_key`'s array
///   (creating the array if missing). Existing entries are preserved; the new
///   value is deduplicated against current contents.
/// - Empty strings, non-string types, and missing `from_key` are no-ops.
///
/// Returns `true` if a value was actually folded (caller may emit a log line).
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

/// Migration steps keyed 1-indexed by `from` version: `MIGRATION_STEPS[n]`
/// is the step from `V_n` to `V_{n+1}`. Slot 0 is a never-invoked
/// placeholder so callers can write `&MIGRATION_STEPS[from..target]`
/// directly — both bounds read as schema-version numbers, no offset math.
///
/// To add a new schema version `V_n`:
/// 1. Add `schema/v{n-1}.rs` with a partial typed lens for the prior shape.
/// 2. Implement `V{n-1}Config::migrate(self) -> Result<toml::Value>`.
/// 3. Bump [`CURRENT_SCHEMA_VERSION`] to `n`.
/// 4. Append a new closure here that deserializes `V{n-1}Config` and calls
///    its `migrate()`. The compile-time assertion below catches drift.
const MIGRATION_STEPS: &[MigrationStep] = &[
    // V0 → V1: padding so slot 0 is never indexed. V0 does not exist.
    |_| unreachable!("MIGRATION_STEPS[0] is a 1-indexing pad and is never invoked"),
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
];

const _: () = assert!(
    MIGRATION_STEPS.len() as u32 == CURRENT_SCHEMA_VERSION,
    "MIGRATION_STEPS must have exactly one entry per schema version \
     (length = CURRENT_SCHEMA_VERSION, including the slot-0 padding)",
);

/// Run the typed migration chain from `from` up to `CURRENT_SCHEMA_VERSION`.
/// `from` must be `< CURRENT_SCHEMA_VERSION` (caller checks).
fn run_chain(value: toml::Value, from: u32) -> Result<toml::Value> {
    run_chain_until(value, from, CURRENT_SCHEMA_VERSION)
}

/// Run the typed migration chain from `from` up to `target` (the shape that
/// is emitted). `target` must be in `from..=CURRENT_SCHEMA_VERSION`.
///
/// Used by `migrate_file` / `migrate_to_current` (target = current) and by
/// [`generate`] (target = any historical version, for fixture generation).
fn run_chain_until(value: toml::Value, from: u32, target: u32) -> Result<toml::Value> {
    if target < from {
        anyhow::bail!("cannot migrate backwards from V{from} to V{target}");
    }
    if target > CURRENT_SCHEMA_VERSION {
        anyhow::bail!(
            "target V{target} exceeds CURRENT_SCHEMA_VERSION (V{CURRENT_SCHEMA_VERSION})"
        );
    }

    let mut cur = value;
    for step in &MIGRATION_STEPS[from as usize..target as usize] {
        cur = step(cur)?;
    }
    Ok(cur)
}

/// Reconcile new typed values into an existing `toml_edit::DocumentMut` so
/// comments and decoration on surviving keys are preserved across save.
///
/// Walks `new` recursively. For each key:
/// - If the key exists in `doc` AND both sides are tables, recurse.
/// - If the key exists in `doc` and at least one side is not a table, replace
///   the value while preserving the key's prefix decor (i.e. the comment lines
///   that lead the key).
/// - If the key does not exist in `doc`, insert it.
///
/// Removed keys (present in `doc` but absent from `new`) are dropped from `doc`.
/// This matches the prior crate behavior: the typed schema is authoritative,
/// and any TOML key not represented in `new` is not part of the current schema.
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
schema_version = 3

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
    fn valid_alias_survives_broken_sibling() {
        let raw = r#"
schema_version = 3

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
schema_version = 3

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
    fn strict_path_still_errors_for_tooling() {
        // `migrate_to_current` stays strict — repair tooling needs the error.
        let raw = r#"
schema_version = 3

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
    fn broken_security_section_is_reported_as_degraded() {
        let raw = r#"
schema_version = 3

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
schema_version = 3

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
schema_version = 3

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
schema_version = 3

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
schema_version = 3

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
}
