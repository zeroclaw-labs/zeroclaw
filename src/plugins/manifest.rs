//! Plugin manifest — the `zeroclaw.plugin.toml` descriptor.
//!
//! Mirrors OpenClaw's `openclaw.plugin.json` but uses TOML to match
//! ZeroClaw's existing config format.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;

use super::traits::PluginCapability;

const SUPPORTED_WIT_MAJOR: u64 = 1;
const SUPPORTED_WIT_PACKAGES: [&str; 3] =
    ["zeroclaw:hooks", "zeroclaw:tools", "zeroclaw:providers"];

/// Filename plugins must use for their manifest.
pub const PLUGIN_MANIFEST_FILENAME: &str = "zeroclaw.plugin.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolManifest {
    pub name: String,
    pub description: String,
    #[serde(default = "default_plugin_tool_parameters")]
    pub parameters: Value,
}

fn default_plugin_tool_parameters() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {}
    })
}

/// Parsed plugin manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Declared capability set for this plugin.
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
    /// Optional module path used by WASM-oriented plugin runtimes.
    #[serde(default)]
    pub module_path: String,
    /// Declared WIT package contracts the plugin expects.
    #[serde(default)]
    pub wit_packages: Vec<String>,
    /// Manifest-declared tools (runtime stub wiring for now).
    #[serde(default)]
    pub tools: Vec<PluginToolManifest>,
    /// Manifest-declared providers (runtime placeholder wiring for now).
    #[serde(default)]
    pub providers: Vec<String>,
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

fn parse_wit_package_version(input: &str) -> anyhow::Result<(&str, u64)> {
    let trimmed = input.trim();
    let (package, version) = trimmed
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("invalid wit package version '{trimmed}'"))?;
    if package.is_empty() || version.is_empty() {
        anyhow::bail!("invalid wit package version '{trimmed}'");
    }
    let major = version
        .split('.')
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid wit package version '{trimmed}'"))?
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid wit package version '{trimmed}'"))?;
    Ok((package, major))
}

pub fn validate_manifest(manifest: &PluginManifest) -> anyhow::Result<()> {
    if manifest.id.trim().is_empty() {
        anyhow::bail!("plugin id cannot be empty");
    }
    if let Some(version) = &manifest.version {
        if version.trim().is_empty() {
            anyhow::bail!("plugin version cannot be empty");
        }
    }
    if manifest.module_path.trim().is_empty() {
        anyhow::bail!("plugin module_path cannot be empty");
    }
    for wit_pkg in &manifest.wit_packages {
        let (package, major) = parse_wit_package_version(wit_pkg)?;
        if !SUPPORTED_WIT_PACKAGES.contains(&package) {
            anyhow::bail!("unsupported wit package '{package}'");
        }
        if major != SUPPORTED_WIT_MAJOR {
            anyhow::bail!(
                "incompatible wit major version for '{package}': expected {SUPPORTED_WIT_MAJOR}, got {major}"
            );
        }
    }
    for tool in &manifest.tools {
        if tool.name.trim().is_empty() {
            anyhow::bail!("plugin tool name cannot be empty");
        }
        if tool.description.trim().is_empty() {
            anyhow::bail!("plugin tool description cannot be empty");
        }
    }
    for provider in &manifest.providers {
        if provider.trim().is_empty() {
            anyhow::bail!("plugin provider name cannot be empty");
        }
    }
    Ok(())
}

impl PluginManifest {
    pub fn is_valid(&self) -> bool {
        validate_manifest(self).is_ok()
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
                assert!(manifest.tools.is_empty());
                assert!(manifest.providers.is_empty());
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

    #[test]
    fn manifest_requires_id_and_module_path_for_runtime_validation() {
        let invalid = PluginManifest::default();
        assert!(!invalid.is_valid());

        let valid = PluginManifest {
            id: "demo".into(),
            name: Some("Demo".into()),
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:hooks@1.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(valid.is_valid());
    }

    #[test]
    fn manifest_rejects_unknown_wit_package() {
        let manifest = PluginManifest {
            id: "demo".into(),
            name: None,
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:unknown@1.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(validate_manifest(&manifest).is_err());
    }
}
