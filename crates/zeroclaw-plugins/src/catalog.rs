//! Read-only package catalog over installed and registry plugin metadata.
//!
//! The host and registry remain the owners of their respective metadata. This
//! module only builds a sorted, per-call view containing references to those
//! canonical records. Package name is the identity boundary; capabilities are
//! attributes of a package, not additional catalog rows.

use crate::PluginInfo;
use crate::registry::{PluginRegistryEntry, PluginRegistryIndex, resolved_entries};
use std::collections::BTreeMap;

/// One package in the materialized plugin catalog.
///
/// Both sources are retained when a package is installed and also appears in
/// the registry. Callers can therefore present version differences without
/// guessing whether an arbitrary registry version string is newer.
#[derive(Debug, Clone, Copy)]
pub struct PluginCatalogEntry<'a> {
    sources: CatalogSources<'a>,
}

#[derive(Debug, Clone, Copy)]
enum CatalogSources<'a> {
    Installed(&'a PluginInfo),
    Available(&'a PluginRegistryEntry),
    InstalledAndAvailable {
        installed: &'a PluginInfo,
        available: &'a PluginRegistryEntry,
    },
}

impl<'a> PluginCatalogEntry<'a> {
    /// Canonical package name used by install, remove, and info commands.
    #[must_use]
    pub fn name(&self) -> &'a str {
        match self.sources {
            CatalogSources::Installed(plugin)
            | CatalogSources::InstalledAndAvailable {
                installed: plugin, ..
            } => plugin.name.as_str(),
            CatalogSources::Available(plugin) => plugin.name.as_str(),
        }
    }

    /// Host-admitted metadata when this package is installed.
    #[must_use]
    pub fn installed(&self) -> Option<&'a PluginInfo> {
        match self.sources {
            CatalogSources::Installed(plugin)
            | CatalogSources::InstalledAndAvailable {
                installed: plugin, ..
            } => Some(plugin),
            CatalogSources::Available(_) => None,
        }
    }

    /// Registry metadata selected by the same unpinned resolution rule as
    /// `plugin install <name>`.
    #[must_use]
    pub fn available(&self) -> Option<&'a PluginRegistryEntry> {
        match self.sources {
            CatalogSources::Available(plugin)
            | CatalogSources::InstalledAndAvailable {
                available: plugin, ..
            } => Some(plugin),
            CatalogSources::Installed(_) => None,
        }
    }
}

/// Merge installed packages with an optional registry index.
///
/// `installed` is the admitted view returned by `PluginHost::list_plugins`.
/// The result has one row per package name and is sorted by name. Registry
/// indexes may contain multiple versions of a package; [`resolved_entries`]
/// selects the same entry an unpinned install would use.
#[must_use]
pub fn package_catalog<'a>(
    installed: &'a [PluginInfo],
    registry: Option<&'a PluginRegistryIndex>,
) -> Vec<PluginCatalogEntry<'a>> {
    let mut entries = BTreeMap::<&'a str, PluginCatalogEntry<'a>>::new();

    for plugin in installed {
        entries.insert(
            plugin.name.as_str(),
            PluginCatalogEntry {
                sources: CatalogSources::Installed(plugin),
            },
        );
    }

    if let Some(registry) = registry {
        for plugin in resolved_entries(registry) {
            if let Some(entry) = entries.get_mut(plugin.name.as_str()) {
                if let Some(installed) = entry.installed() {
                    entry.sources = CatalogSources::InstalledAndAvailable {
                        installed,
                        available: plugin,
                    };
                }
            } else {
                entries.insert(
                    plugin.name.as_str(),
                    PluginCatalogEntry {
                        sources: CatalogSources::Available(plugin),
                    },
                );
            }
        }
    }

    entries.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginCapability, PluginPermission};
    use std::path::PathBuf;

    fn installed(name: &str, version: &str) -> PluginInfo {
        PluginInfo {
            name: name.to_string(),
            version: version.to_string(),
            description: Some(format!("installed {name}")),
            capabilities: vec![PluginCapability::Tool, PluginCapability::Skill],
            permissions: vec![PluginPermission::ConfigRead],
            wasm_path: Some(PathBuf::from("plugin.wasm")),
            loaded: true,
        }
    }

    fn available(name: &str, version: &str) -> PluginRegistryEntry {
        PluginRegistryEntry {
            name: name.to_string(),
            version: version.to_string(),
            description: Some(format!("available {name}")),
            author: None,
            capabilities: vec!["tool".to_string(), "skill".to_string()],
            url: format!("https://example.invalid/{name}-{version}.zip"),
            sha256: None,
        }
    }

    #[test]
    fn empty_sources_produce_an_empty_catalog() {
        assert!(package_catalog(&[], None).is_empty());
    }

    #[test]
    fn package_identity_does_not_expand_capabilities_into_rows() {
        let installed = [installed("multi-capability", "0.1.0")];

        let catalog = package_catalog(&installed, None);

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name(), "multi-capability");
        assert_eq!(
            catalog[0]
                .installed()
                .map(|plugin| plugin.capabilities.as_slice()),
            Some(installed[0].capabilities.as_slice())
        );
    }

    #[test]
    fn installed_and_registry_records_are_retained_without_copying_metadata() {
        let installed = [installed("calendar", "0.1.0")];
        let registry = PluginRegistryIndex {
            plugins: vec![available("calendar", "0.2.0")],
            registry_url: None,
        };

        let catalog = package_catalog(&installed, Some(&registry));
        let entry = &catalog[0];

        assert_eq!(entry.name(), "calendar");
        assert_eq!(
            entry.installed().map(|plugin| plugin.version.as_str()),
            Some("0.1.0")
        );
        assert_eq!(
            entry.available().map(|plugin| plugin.version.as_str()),
            Some("0.2.0")
        );
    }

    #[test]
    fn registry_selection_matches_unpinned_install_and_output_is_sorted() {
        let installed = [installed("zebra", "1.0.0")];
        let registry = PluginRegistryIndex {
            plugins: vec![
                available("calendar", "0.1.0"),
                available("alpha", "1.0.0"),
                available("calendar", "0.2.0"),
            ],
            registry_url: None,
        };

        let catalog = package_catalog(&installed, Some(&registry));

        assert_eq!(
            catalog
                .iter()
                .map(PluginCatalogEntry::name)
                .collect::<Vec<_>>(),
            ["alpha", "calendar", "zebra"]
        );
        assert_eq!(
            catalog[1].available().map(|plugin| plugin.version.as_str()),
            Some("0.2.0")
        );
    }
}
