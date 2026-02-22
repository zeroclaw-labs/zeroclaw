use std::collections::{HashMap, HashSet};

use super::manifest::{PluginManifest, PluginToolManifest};

#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    manifests: HashMap<String, PluginManifest>,
    tools: Vec<PluginToolManifest>,
    providers: HashSet<String>,
    tool_modules: HashMap<String, String>,
    provider_modules: HashMap<String, String>,
}

impl PluginRegistry {
    pub fn register(&mut self, manifest: PluginManifest) {
        let module_path = manifest.module_path.clone();
        self.tools.extend(manifest.tools.iter().cloned());
        for tool in &manifest.tools {
            self.tool_modules
                .entry(tool.name.clone())
                .or_insert_with(|| module_path.clone());
        }
        for provider in &manifest.providers {
            self.providers.insert(provider.trim().to_string());
            self.provider_modules
                .entry(provider.trim().to_string())
                .or_insert_with(|| module_path.clone());
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

    pub fn tool_module_path(&self, tool: &str) -> Option<&str> {
        self.tool_modules.get(tool).map(String::as_str)
    }

    pub fn provider_module_path(&self, provider: &str) -> Option<&str> {
        self.provider_modules.get(provider).map(String::as_str)
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
