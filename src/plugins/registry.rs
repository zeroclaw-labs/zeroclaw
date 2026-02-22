use std::collections::{HashMap, HashSet};

use super::manifest::{PluginManifest, PluginToolManifest};

#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    manifests: HashMap<String, PluginManifest>,
    tools: Vec<PluginToolManifest>,
    providers: HashSet<String>,
}

impl PluginRegistry {
    pub fn register(&mut self, manifest: PluginManifest) {
        self.tools.extend(manifest.tools.iter().cloned());
        for provider in &manifest.providers {
            self.providers.insert(provider.trim().to_string());
        }
        self.manifests.insert(manifest.id.clone(), manifest);
    }

    pub fn hooks(&self) -> Vec<&PluginManifest> {
        self.manifests.values().collect()
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn tools(&self) -> &[PluginToolManifest] {
        &self.tools
    }

    pub fn has_provider(&self, name: &str) -> bool {
        self.providers.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_registry_empty_by_default() {
        let reg = PluginRegistry::default();
        assert!(reg.hooks().is_empty());
        assert!(reg.tools().is_empty());
        assert!(!reg.has_provider("demo"));
    }
}
