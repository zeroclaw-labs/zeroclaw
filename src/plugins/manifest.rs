//! Plugin manifest — the `zeroclaw.plugin.toml` descriptor.
//!
//! Mirrors OpenClaw's `openclaw.plugin.json` but uses TOML to match
//! ZeroClaw's existing config format.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::traits::PluginCapability;

const SUPPORTED_WIT_MAJOR: u64 = 1;
const SUPPORTED_WIT_PACKAGES: [&str; 3] =
    ["zeroclaw:hooks", "zeroclaw:tools", "zeroclaw:providers"];

/// Validation profile for plugin manifests.
///
/// Runtime uses `RuntimeWasm` today (strict; requires module path).
/// `SchemaOnly` exists so future non-WASM plugin forms can validate metadata
/// without forcing a fake module path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestValidationProfile {
    RuntimeWasm,
    SchemaOnly,
}

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
    /// WASM module path used by runtime execution.
    /// Required in runtime validation; optional in schema-only validation.
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

fn required_wit_package_for_capability(capability: &PluginCapability) -> &'static str {
    match capability {
        PluginCapability::Hooks | PluginCapability::ModifyToolResults => "zeroclaw:hooks",
        PluginCapability::Tools => "zeroclaw:tools",
        PluginCapability::Providers => "zeroclaw:providers",
    }
}

pub fn validate_manifest_with_profile(
    manifest: &PluginManifest,
    profile: ManifestValidationProfile,
) -> anyhow::Result<()> {
    if manifest.id.trim().is_empty() {
        anyhow::bail!("plugin id cannot be empty");
    }
    if let Some(version) = &manifest.version {
        if version.trim().is_empty() {
            anyhow::bail!("plugin version cannot be empty");
        }
    }
    if matches!(profile, ManifestValidationProfile::RuntimeWasm)
        && manifest.module_path.trim().is_empty()
    {
        anyhow::bail!("plugin module_path cannot be empty");
    }
    let mut declared_wit_packages = HashSet::new();
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
        declared_wit_packages.insert(package.to_string());
    }
    if manifest
        .capabilities
        .contains(&PluginCapability::ModifyToolResults)
        && !manifest.capabilities.contains(&PluginCapability::Hooks)
    {
        anyhow::bail!(
            "plugin capability 'ModifyToolResults' requires declaring 'Hooks' capability"
        );
    }
    for capability in &manifest.capabilities {
        let required_package = required_wit_package_for_capability(capability);
        if !declared_wit_packages.contains(required_package) {
            anyhow::bail!(
                "plugin capability '{capability:?}' requires wit package '{required_package}@{SUPPORTED_WIT_MAJOR}.x'"
            );
        }
    }
    if !manifest.tools.is_empty() && !declared_wit_packages.contains("zeroclaw:tools") {
        anyhow::bail!("plugin tools require wit package 'zeroclaw:tools@{SUPPORTED_WIT_MAJOR}.x'");
    }
    if !manifest.providers.is_empty() && !declared_wit_packages.contains("zeroclaw:providers") {
        anyhow::bail!(
            "plugin providers require wit package 'zeroclaw:providers@{SUPPORTED_WIT_MAJOR}.x'"
        );
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

pub fn validate_manifest(manifest: &PluginManifest) -> anyhow::Result<()> {
    validate_manifest_with_profile(manifest, ManifestValidationProfile::RuntimeWasm)
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

    #[test]
    fn manifest_rejects_empty_module_path() {
        let manifest = PluginManifest {
            id: "demo".into(),
            name: None,
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![],
            module_path: "   ".into(),
            wit_packages: vec!["zeroclaw:hooks@1.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn schema_only_validation_allows_empty_module_path() {
        let manifest = PluginManifest {
            id: "demo".into(),
            name: None,
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![],
            module_path: "   ".into(),
            wit_packages: vec![],
            tools: vec![],
            providers: vec![],
        };
        assert!(
            validate_manifest_with_profile(&manifest, ManifestValidationProfile::SchemaOnly)
                .is_ok()
        );
    }

    #[test]
    fn manifest_rejects_capability_without_matching_wit_package() {
        let manifest = PluginManifest {
            id: "demo".into(),
            name: None,
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![PluginCapability::Tools],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:hooks@1.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn manifest_rejects_modify_tool_results_without_hooks_capability() {
        let manifest = PluginManifest {
            id: "demo".into(),
            name: None,
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![PluginCapability::ModifyToolResults],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:hooks@1.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn manifest_rejects_tools_without_tools_wit_package() {
        let manifest = PluginManifest {
            id: "demo".into(),
            name: None,
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:hooks@1.0.0".into()],
            tools: vec![PluginToolManifest {
                name: "demo_tool".into(),
                description: "demo tool".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            }],
            providers: vec![],
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn manifest_rejects_providers_without_providers_wit_package() {
        let manifest = PluginManifest {
            id: "demo".into(),
            name: None,
            description: None,
            version: Some("1.0.0".into()),
            config_schema: None,
            capabilities: vec![],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:hooks@1.0.0".into()],
            tools: vec![],
            providers: vec!["demo_provider".into()],
        };
        assert!(validate_manifest(&manifest).is_err());
    }
}
