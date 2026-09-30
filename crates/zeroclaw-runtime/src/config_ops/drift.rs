//! Config drift: which properties differ between the live in-memory config
//! and the canonical file on disk. Shared by the gateway's
//! `GET /api/config/drift` and the RPC `config/drift` method.

use serde::{Deserialize, Serialize};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

fn is_false(b: &bool) -> bool {
    !*b
}

/// One drift entry surfaced when in-memory Config diverges from the on-disk file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DriftEntry {
    pub path: String,
    /// `true` for secret fields where values cannot be exposed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub secret: bool,
    /// Always `true` when surfaced. Present so secret entries unambiguously
    /// communicate the drift signal in shape `{path, secret: true, drifted: true}`.
    pub drifted: bool,
    /// In-memory value (the daemon's view). Absent for secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_memory_value: Option<serde_json::Value>,
    /// On-disk value (what the file contains right now). Absent for secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_disk_value: Option<serde_json::Value>,
}

/// Fields the gateway owns end-to-end (mints, rotates, persists itself).
/// They're skipped by [`compute_drift`] so the dashboard doesn't surface a
/// banner the operator can't act on. Add new entries here when a similar
/// gateway-managed field lands (e.g. webhook secret rotation).
pub fn is_gateway_managed_field(name: &str) -> bool {
    // Match the prop-field name actually emitted by the `Configurable` derive,
    // which preserves the Rust field's snake_case (`paired_tokens`), not kebab.
    matches!(name, "gateway.paired_tokens")
}

pub async fn compute_drift(in_memory: &zeroclaw_config::schema::Config) -> Vec<DriftEntry> {
    try_compute_drift(in_memory).await.unwrap_or_default()
}

pub async fn try_compute_drift(
    in_memory: &zeroclaw_config::schema::Config,
) -> Result<Vec<DriftEntry>, ConfigApiError> {
    let path = &in_memory.config_path;
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(ConfigApiError::new(
                ConfigApiCode::ConfigChangedExternally,
                format!("cannot inspect the canonical config before comparing changes: {error}"),
            ));
        }
    };

    // Re-parse the on-disk form into a fresh Config for value-by-value comparison.
    let mut on_disk = toml::from_str::<zeroclaw_config::schema::Config>(&raw).map_err(|_| {
        ConfigApiError::new(
            ConfigApiCode::ConfigChangedExternally,
            "cannot compare changes because the canonical config is malformed",
        )
    })?;
    on_disk.config_path = path.clone();

    let in_memory_props: std::collections::HashMap<String, zeroclaw_config::traits::PropFieldInfo> =
        in_memory
            .prop_fields()
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();
    let on_disk_props: std::collections::HashMap<String, zeroclaw_config::traits::PropFieldInfo> =
        on_disk
            .prop_fields()
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();

    let mut drift: Vec<DriftEntry> = Vec::new();
    let mut all_names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    all_names.extend(in_memory_props.keys().map(String::as_str));
    all_names.extend(on_disk_props.keys().map(String::as_str));
    for name in all_names {
        if is_gateway_managed_field(name) {
            continue;
        }
        // Env overrides (`ZEROCLAW_<path>`) apply in memory but never persist to
        // disk, so a disk comparison always reports drift the operator can't fix.
        if in_memory.prop_is_env_overridden(name) {
            continue;
        }
        let mem = in_memory_props.get(name);
        let disk = on_disk_props.get(name);
        let mem_display = mem
            .map(|p| p.display_value.as_str())
            .unwrap_or(zeroclaw_config::traits::UNSET_DISPLAY);
        let disk_display = disk
            .map(|p| p.display_value.as_str())
            .unwrap_or(zeroclaw_config::traits::UNSET_DISPLAY);
        if mem_display == disk_display {
            continue;
        }
        let is_sensitive = mem
            .or(disk)
            .map(|p| p.is_secret || p.derived_from_secret)
            .unwrap_or(false);
        if is_sensitive {
            use sha2::{Digest, Sha256};
            let mem_hash = Sha256::digest(mem_display.as_bytes());
            let disk_hash = Sha256::digest(disk_display.as_bytes());
            if mem_hash == disk_hash {
                continue;
            }
            drift.push(DriftEntry {
                path: name.to_string(),
                secret: true,
                drifted: true,
                in_memory_value: None,
                on_disk_value: None,
            });
        } else {
            drift.push(DriftEntry {
                path: name.to_string(),
                secret: false,
                drifted: true,
                in_memory_value: Some(serde_json::Value::String(mem_display.to_string())),
                on_disk_value: Some(serde_json::Value::String(disk_display.to_string())),
            });
        }
    }

    // Stable order so callers can diff snapshots.
    drift.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(drift)
}
