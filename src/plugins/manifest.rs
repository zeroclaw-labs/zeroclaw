//! Plugin manifest — the `zeroclaw.plugin.toml` descriptor.
//!
//! Mirrors OpenClaw's `openclaw.plugin.json` but uses TOML to match
//! ZeroClaw's existing config format.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Filename plugins must use for their manifest.
pub const PLUGIN_MANIFEST_FILENAME: &str = "zeroclaw.plugin.toml";

/// Parsed plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier (e.g. `"hello-world"`).
    pub id: String,
    /// Human-readable name.
    pub name: Option<String>,
    /// Short description.
    pub description: Option<String>,
    /// SemVer version string.
    pub version: Option<String>,
    /// Optional JSON-Schema-style config descriptor (stored as TOML table).
    pub config_schema: Option<toml::Value>,
}

/// Result of attempting to load a manifest from a directory.
pub enum ManifestLoadResult {
    Ok {
        manifest: PluginManifest,
        path: std::path::PathBuf,
    },
    Err {
        error: String,
        path: std::path::PathBuf,
    },
}

/// Load and parse `zeroclaw.plugin.toml` from `root_dir`.
pub fn load_manifest(root_dir: &Path) -> ManifestLoadResult {
    let manifest_path = root_dir.join(PLUGIN_MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return ManifestLoadResult::Err {
            error: format!("manifest not found: {}", manifest_path.display()),
            path: manifest_path,
        };
    }
    let raw = match fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(e) => {
            return ManifestLoadResult::Err {
                error: format!("failed to read manifest: {e}"),
                path: manifest_path,
            }
        }
    };
    match toml::from_str::<PluginManifest>(&raw) {
        Ok(manifest) => {
            if manifest.id.trim().is_empty() {
                return ManifestLoadResult::Err {
                    error: "manifest requires non-empty `id`".into(),
                    path: manifest_path,
                };
            }
            ManifestLoadResult::Ok {
                manifest,
                path: manifest_path,
            }
        }
        Err(e) => ManifestLoadResult::Err {
            error: format!("failed to parse manifest: {e}"),
            path: manifest_path,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn load_valid_manifest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(PLUGIN_MANIFEST_FILENAME),
            r#"
id = "test-plugin"
name = "Test Plugin"
description = "A test"
version = "0.1.0"
"#,
        )
        .unwrap();

        match load_manifest(dir.path()) {
            ManifestLoadResult::Ok { manifest, .. } => {
                assert_eq!(manifest.id, "test-plugin");
                assert_eq!(manifest.name.as_deref(), Some("Test Plugin"));
            }
            ManifestLoadResult::Err { error, .. } => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn load_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        match load_manifest(dir.path()) {
            ManifestLoadResult::Err { error, .. } => {
                assert!(error.contains("not found"));
            }
            ManifestLoadResult::Ok { .. } => panic!("should fail"),
        }
    }

    #[test]
    fn load_manifest_missing_id() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(PLUGIN_MANIFEST_FILENAME),
            r#"
name = "No ID"
"#,
        )
        .unwrap();

        match load_manifest(dir.path()) {
            ManifestLoadResult::Err { error, .. } => {
                assert!(error.contains("missing field `id`") || error.contains("requires"));
            }
            ManifestLoadResult::Ok { .. } => panic!("should fail"),
        }
    }

    #[test]
    fn load_manifest_empty_id() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(PLUGIN_MANIFEST_FILENAME),
            r#"
id = "  "
"#,
        )
        .unwrap();

        match load_manifest(dir.path()) {
            ManifestLoadResult::Err { error, .. } => {
                assert!(error.contains("non-empty"));
            }
            ManifestLoadResult::Ok { .. } => panic!("should fail"),
        }
    }
}
