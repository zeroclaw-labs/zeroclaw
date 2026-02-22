use std::collections::HashMap;

use super::manifest::PluginManifest;

#[derive(Debug, Default)]
pub struct PluginRegistry {
    manifests: HashMap<String, PluginManifest>,
}

impl PluginRegistry {
    pub fn register(&mut self, manifest: PluginManifest) {
        self.manifests.insert(manifest.id.clone(), manifest);
    }

    pub fn hooks(&self) -> Vec<&PluginManifest> {
        self.manifests.values().collect()
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_registry_empty_by_default() {
        let reg = PluginRegistry::default();
        assert!(reg.hooks().is_empty());
    }
}
