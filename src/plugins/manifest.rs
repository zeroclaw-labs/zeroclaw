use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::traits::PluginCapability;

const SUPPORTED_WIT_MAJOR: u64 = 1;
const SUPPORTED_WIT_PACKAGES: [&str; 3] =
    ["zeroclaw:hooks", "zeroclaw:tools", "zeroclaw:providers"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
    #[serde(default)]
    pub module_path: String,
    #[serde(default)]
    pub wit_packages: Vec<String>,
    #[serde(default)]
    pub tools: Vec<PluginToolManifest>,
    #[serde(default)]
    pub providers: Vec<String>,
}

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
    if manifest.version.trim().is_empty() {
        anyhow::bail!("plugin version cannot be empty");
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

    #[test]
    fn manifest_requires_id_and_version() {
        let invalid = PluginManifest::default();
        assert!(!invalid.is_valid());

        let valid = PluginManifest {
            id: "demo".into(),
            version: "1.0.0".into(),
            capabilities: vec![],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:hooks@1.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(valid.is_valid());
    }

    #[test]
    fn manifest_rejects_incompatible_wit_major() {
        let manifest = PluginManifest {
            id: "demo".into(),
            version: "1.0.0".into(),
            capabilities: vec![],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:hooks@2.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn manifest_rejects_unknown_wit_package() {
        let manifest = PluginManifest {
            id: "demo".into(),
            version: "1.0.0".into(),
            capabilities: vec![],
            module_path: "plugins/demo.wasm".into(),
            wit_packages: vec!["zeroclaw:unknown@1.0.0".into()],
            tools: vec![],
            providers: vec![],
        };
        assert!(validate_manifest(&manifest).is_err());
    }
}
