use anyhow::{Context, Result};
use std::path::Path;

use crate::schema::Config;
use crate::schema::v1::V1Config;
use crate::schema::v2::V2Config;

/// The schema version this binary writes and expects on disk.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

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
}

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
                // Whole-config loss degrades the security posture: every
                // security-critical section is gone, so mark it so the serving
                // gate refuses to start without an explicit override.
                dropped_security: vec![WHOLE_CONFIG_SENTINEL.to_string()],
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
    fn partial_telegram_alias_survives_salvage() {
        // A Telegram alias with no `bot_token` (e.g. just created via
        // create_map_key, then round-tripped through save_dirty's
        // prune_empty_leaves, which strips the empty string) must survive
        // salvage instead of being dropped: `bot_token` now has
        // `#[serde(default)]`, so a missing token deserializes the same as
        // an explicit `bot_token = ""`. Runtime safety is enforced
        // separately by `validate_bot_token` when `enabled = true`.
        let raw = r#"
schema_version = 3

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
schema_version = 3

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
schema_version = 3

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
schema_version = 3

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
schema_version = 3

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
    fn provider_pruner_never_panics_on_non_table_shapes() {
        // Array-of-tables where a family map is expected, scalar [providers],
        // array alias value. The salvage path is the daemon's never-fail
        // loader, and prune_bad_provider_aliases carries expect() calls that
        // rely on the scalar pre-passes; pin that invariant here.
        for raw in [
            "schema_version = 3\nproviders = 3\n",
            "schema_version = 3\n[[providers.models.ollama]]\nmodel = \"x\"\n",
            "schema_version = 3\n[providers.models.ollama]\nai = [1, 2]\n",
            "schema_version = 3\n[providers.models]\nollama = [1]\n",
        ] {
            let _ = migrate_to_current_salvaged(raw);
        }
    }

    #[test]
    fn scalar_provider_nodes_pruned_without_sinking_section() {
        // A scalar where a family/kind table is required must drop only
        // that node, not the whole [providers] section.
        let raw = r#"
schema_version = 3

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
        let raw = "schema_version = 3\nthis_is_a_bare_top_level = \"value\"\n[\n";
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
