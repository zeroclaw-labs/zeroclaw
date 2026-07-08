use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const REGISTRY_CACHE_DIR_NAME: &str = "plugin-registry";
pub const REGISTRY_CACHE_FILE_NAME: &str = "registry.json";

/// Metadata entry from an installable plugin registry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRegistryEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Installable plugin registry index.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRegistryIndex {
    #[serde(default)]
    pub plugins: Vec<PluginRegistryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_url: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PluginSpec {
    pub name: String,
    pub version: Option<String>,
}

pub fn search_entries<'a>(
    index: &'a PluginRegistryIndex,
    query: &str,
) -> Vec<&'a PluginRegistryEntry> {
    let query = query.to_lowercase();
    index
        .plugins
        .iter()
        .filter(|entry| {
            entry.name.to_lowercase().contains(&query)
                || entry
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
        })
        .collect()
}

pub fn registry_cache_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join(REGISTRY_CACHE_DIR_NAME)
        .join(REGISTRY_CACHE_FILE_NAME)
}

pub fn read_cached_registry_index(data_dir: &Path) -> Result<Option<PluginRegistryIndex>> {
    let path = registry_cache_path(data_dir);
    let Ok(registry_json) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    serde_json::from_str::<PluginRegistryIndex>(&registry_json)
        .map(Some)
        .with_context(|| format!("parsing cached plugin registry {}", path.display()))
}

pub fn write_cached_registry_index(
    data_dir: &Path,
    registry_url: &str,
    index: &PluginRegistryIndex,
) -> Result<()> {
    let path = registry_cache_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating plugin registry cache {}", parent.display()))?;
    }
    let mut cached = index.clone();
    cached.registry_url = Some(registry_url.to_string());
    let registry_json = serde_json::to_string_pretty(&cached)
        .context("serializing cached plugin registry metadata")?;
    std::fs::write(&path, registry_json)
        .with_context(|| format!("writing plugin registry cache {}", path.display()))
}

pub fn install_source(entry: &PluginRegistryEntry) -> String {
    format!("{}@{}", entry.name, entry.version)
}

pub fn install_command(entry: &PluginRegistryEntry, registry_url: Option<&str>) -> String {
    let source = install_source(entry);
    match registry_url {
        Some(registry_url) if !registry_url.trim().is_empty() => {
            format!("zeroclaw plugin install {source} --registry {registry_url}")
        }
        _ => format!("zeroclaw plugin install {source}"),
    }
}

pub fn parse_plugin_spec(source: &str) -> Result<PluginSpec> {
    let source = source.trim();
    if source.is_empty() {
        bail!("plugin name must not be empty");
    }
    let Some((name, version)) = source.rsplit_once('@') else {
        return Ok(PluginSpec {
            name: source.to_string(),
            version: None,
        });
    };
    if name.is_empty() || version.is_empty() {
        bail!("plugin registry source must be name or name@version");
    }
    Ok(PluginSpec {
        name: name.to_string(),
        version: Some(version.to_string()),
    })
}

pub fn resolve_entry<'a>(
    index: &'a PluginRegistryIndex,
    spec: &PluginSpec,
) -> Result<&'a PluginRegistryEntry> {
    let mut matches = index
        .plugins
        .iter()
        .filter(|entry| entry.name == spec.name)
        .filter(|entry| {
            spec.version
                .as_ref()
                .is_none_or(|version| &entry.version == version)
        });
    matches
        .next_back()
        .ok_or_else(|| anyhow::Error::msg(format!("plugin '{}' not found in registry", spec.name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_index() -> PluginRegistryIndex {
        PluginRegistryIndex {
            plugins: vec![
                PluginRegistryEntry {
                    name: "team-calendar".to_string(),
                    version: "0.1.0".to_string(),
                    description: Some("Schedule meetings".to_string()),
                    author: None,
                    capabilities: vec!["tool".to_string()],
                    url: "https://example.invalid/team-calendar-0.1.0.zip".to_string(),
                    sha256: None,
                },
                PluginRegistryEntry {
                    name: "web-research".to_string(),
                    version: "0.1.0".to_string(),
                    description: Some("Research web pages".to_string()),
                    author: None,
                    capabilities: vec!["tool".to_string()],
                    url: "https://example.invalid/web-research-0.1.0.zip".to_string(),
                    sha256: None,
                },
                PluginRegistryEntry {
                    name: "team-calendar".to_string(),
                    version: "0.2.0".to_string(),
                    description: Some("Team calendar scheduling".to_string()),
                    author: None,
                    capabilities: vec!["tool".to_string()],
                    url: "https://example.invalid/team-calendar-0.2.0.zip".to_string(),
                    sha256: None,
                },
            ],
            registry_url: None,
        }
    }

    #[test]
    fn search_matches_name_or_description() {
        let index = sample_index();

        let matches = search_entries(&index, "calendar");

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].name, "team-calendar");
        assert_eq!(matches[1].version, "0.2.0");
    }

    #[test]
    fn resolves_pinned_version_or_latest_listed_match() {
        let index = sample_index();

        let latest = resolve_entry(
            &index,
            &PluginSpec {
                name: "team-calendar".to_string(),
                version: None,
            },
        )
        .unwrap();
        let pinned = resolve_entry(
            &index,
            &PluginSpec {
                name: "team-calendar".to_string(),
                version: Some("0.1.0".to_string()),
            },
        )
        .unwrap();

        assert_eq!(latest.version, "0.2.0");
        assert_eq!(pinned.version, "0.1.0");
        assert_eq!(
            parse_plugin_spec("team-calendar@0.2.0").unwrap(),
            PluginSpec {
                name: "team-calendar".to_string(),
                version: Some("0.2.0".to_string()),
            }
        );
    }

    #[test]
    fn writes_and_reads_cached_registry_with_origin() {
        let dir = tempfile::tempdir().unwrap();
        let index = sample_index();

        write_cached_registry_index(dir.path(), "https://example.invalid/registry.json", &index)
            .unwrap();
        let cached = read_cached_registry_index(dir.path())
            .unwrap()
            .expect("cache should exist");

        assert_eq!(cached.plugins, index.plugins);
        assert_eq!(
            cached.registry_url.as_deref(),
            Some("https://example.invalid/registry.json")
        );
        assert!(registry_cache_path(dir.path()).is_file());
    }
}
