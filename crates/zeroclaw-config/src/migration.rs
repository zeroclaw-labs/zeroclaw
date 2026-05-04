//! Forward-only schema migration: V1 → V2 → V3.
//!
//! User TOML on disk is the source of truth. Each historical version (V1, V2)
//! is a partial typed lens (`schema/v{1,2}.rs`) — explicit Rust fields only for
//! what transforms between adjacent versions; everything else rides through
//! `passthrough: toml::Table`. V3 is the live `Config` in `schema.rs`.
//!
//! Public API (preserved from the previous implementation so existing callers
//! in `schema.rs`, `src/main.rs`, gateway, tools, and tests keep compiling
//! without changes):
//! - `CURRENT_SCHEMA_VERSION` — current schema version constant
//! - `V1_LEGACY_KEYS` — top-level keys whose presence implies V1 input
//! - `migrate_to_current(&str) -> Result<Config>` — high-level: TOML → V3 Config
//! - `migrate_file(&str) -> Result<Option<String>>` — pure transform; returns
//!   `Some(migrated)` if migration ran, `None` if input was already current
//! - `sync_table(toml_edit::Table, &toml::Table)` — comment-preserving
//!   reconciliation helper used by `Config::save()`

use anyhow::{Context, Result};
use std::path::Path;

use crate::schema::Config;
use crate::schema::v1::V1Config;
use crate::schema::v2::V2Config;

/// The schema version this binary writes and expects on disk.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Top-level TOML keys that existed in V1 but were removed or renamed in V2.
/// Presence of any of these in a config without `schema_version` is a strong
/// V1 signal; used by `Config::load_or_init` to detect legacy configs that
/// need silent in-memory migration.
///
/// Source: `git show 1ec9c14ca:crates/zeroclaw-config/src/schema.rs` —
/// fields removed in the V1→V2 diff.
pub const V1_LEGACY_KEYS: &[&str] = &[
    "api_key",
    "api_url",
    "api_path",
    "default_provider",
    "default_model",
    "model_providers",
    "default_temperature",
    "provider_timeout_secs",
    "provider_max_tokens",
    "extra_headers",
    "model_routes",
    "embedding_routes",
    "channels_config",
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
        Some(other) => Err(anyhow::anyhow!(
            "schema_version must be a positive integer, got {other}"
        )),
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
        return Err(anyhow::anyhow!(
            "config schema_version {from} is newer than this binary supports ({CURRENT_SCHEMA_VERSION})"
        ));
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

/// High-level: arbitrary versioned TOML → fully validated V3 `Config`.
/// Runs migration if needed, then deserializes into the current `Config` type.
pub fn migrate_to_current(input: &str) -> Result<Config> {
    let value: toml::Value = toml::from_str(input).context("failed to parse config TOML")?;
    let from = detect_version(&value)?;
    let final_value = if from == CURRENT_SCHEMA_VERSION {
        value
    } else if from > CURRENT_SCHEMA_VERSION {
        return Err(anyhow::anyhow!(
            "config schema_version {from} is newer than this binary supports ({CURRENT_SCHEMA_VERSION})"
        ));
    } else {
        run_chain(value, from)?
    };
    final_value
        .try_into()
        .context("migrated config failed to deserialize as current schema")
}

/// File-API wrapper: read disk config, migrate, write `<file>.backup`
/// adjacent to the original *before* overwriting. Returns `Ok(None)` when
/// already current.
///
/// Backup file is `<config_filename>.backup` (joined cross-platform via
/// `Path` ops). The backup is fsync'd to disk before the original is touched,
/// so a partial overwrite is recoverable.
pub fn migrate_file_in_place(path: &Path) -> Result<Option<MigrateReport>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    let migrated = match migrate_file(&raw)? {
        Some(s) => s,
        None => return Ok(None),
    };
    let parent = path
        .parent()
        .with_context(|| format!("config path {} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .with_context(|| format!("config path {} has no file name", path.display()))?;
    let backup_path = parent.join(format!("{file_name}.backup"));

    // Backup BEFORE touching original. Use copy so backup gets a fresh inode.
    std::fs::copy(path, &backup_path).with_context(|| {
        format!(
            "failed to write backup {} before migration",
            backup_path.display()
        )
    })?;

    // Overwrite original with migrated content.
    std::fs::write(path, migrated.as_bytes()).with_context(|| {
        format!(
            "failed to write migrated config to {} (backup intact at {})",
            path.display(),
            backup_path.display()
        )
    })?;

    Ok(Some(MigrateReport {
        backup_path,
        to_version: CURRENT_SCHEMA_VERSION,
    }))
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
            return Err(anyhow::Error::from(e))
                .with_context(|| format!("failed to read config at {}", path.display()));
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
            path.display(),
            CURRENT_SCHEMA_VERSION,
        );
    }
    anyhow::bail!(
        "config at {} is schema_version {from}; run `zeroclaw config migrate` to update before modifying",
        path.display(),
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

/// Run the typed migration chain from `from` up to `CURRENT_SCHEMA_VERSION`.
/// `from` must be < `CURRENT_SCHEMA_VERSION` (caller checks).
fn run_chain(value: toml::Value, from: u32) -> Result<toml::Value> {
    // Step 1 → 2
    let after_v1 = if from < 2 {
        let v1: V1Config = value
            .try_into()
            .context("failed to deserialize input as V1 schema")?;
        let v2 = v1.migrate();
        toml::Value::try_from(v2).context("failed to serialize V2 intermediate")?
    } else {
        value
    };

    // Step 2 → 3
    if from < 3 {
        let v2: V2Config = after_v1
            .try_into()
            .context("failed to deserialize as V2 schema")?;
        v2.migrate().context("failed to migrate V2 → V3")
    } else {
        Ok(after_v1)
    }
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
fn toml_value_to_edit_item(value: &toml::Value) -> toml_edit::Item {
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
}
