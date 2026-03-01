//! Plugin registry — collects loaded plugins, their tools, hooks, and diagnostics.
//!
//! Mirrors OpenClaw's `PluginRegistry` / `createPluginRegistry()`.

use std::collections::{HashMap, HashSet};

use crate::hooks::HookHandler;
use crate::tools::traits::Tool;

use super::manifest::{PluginManifest, PluginToolManifest};

/// Status of a loaded plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    /// Successfully registered.
    Active,
    /// Disabled via config.
    Disabled,
    /// Failed during loading or registration.
    Error(String),
}

/// Origin of a discovered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginOrigin {
    /// Shipped with the binary.
    Bundled,
    /// Found in `~/.zeroclaw/extensions/`.
    Global,
    /// Found in `<workspace>/.zeroclaw/extensions/`.
    Workspace,
}

/// Record for a single loaded plugin.
#[derive(Debug, Clone)]
pub struct PluginRecord {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub source: String,
    pub origin: PluginOrigin,
    pub status: PluginStatus,
}

/// Diagnostic emitted during plugin discovery or loading.
#[derive(Debug, Clone)]
pub struct PluginDiagnostic {
    pub level: DiagnosticLevel,
    pub plugin_id: Option<String>,
    pub source: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warn,
    Error,
}

/// Registration of a tool contributed by a plugin.
pub struct PluginToolRegistration {
    pub plugin_id: String,
    pub tool: Box<dyn Tool>,
}

/// Registration of a hook contributed by a plugin.
pub struct PluginHookRegistration {
    pub plugin_id: String,
    pub handler: Box<dyn HookHandler>,
}

/// The plugin registry — the central collection of everything plugins contribute.
///
/// Analogous to OpenClaw's `PluginRegistry` returned by `loadPlugins()`.
pub struct PluginRegistry {
    pub plugins: Vec<PluginRecord>,
    pub tools: Vec<PluginToolRegistration>,
    pub hooks: Vec<PluginHookRegistration>,
    pub diagnostics: Vec<PluginDiagnostic>,
    manifests: HashMap<String, PluginManifest>,
    manifest_tools: Vec<PluginToolManifest>,
    manifest_providers: HashSet<String>,
    tool_modules: HashMap<String, String>,
    provider_modules: HashMap<String, String>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            tools: Vec::new(),
            hooks: Vec::new(),
            diagnostics: Vec::new(),
            manifests: HashMap::new(),
            manifest_tools: Vec::new(),
            manifest_providers: HashSet::new(),
            tool_modules: HashMap::new(),
            provider_modules: HashMap::new(),
        }
    }

    /// Number of active (successfully loaded) plugins.
    pub fn active_count(&self) -> usize {
        self.plugins
            .iter()
            .filter(|p| p.status == PluginStatus::Active)
            .count()
    }

    /// Push a diagnostic message.
    pub fn push_diagnostic(&mut self, diag: PluginDiagnostic) {
        self.diagnostics.push(diag);
    }

    /// Register a manifest for lightweight runtime routing lookups.
    pub fn register(&mut self, manifest: PluginManifest) {
        self.manifests.insert(manifest.id.clone(), manifest);
        self.rebuild_indexes();
    }

    /// Backward-compat alias retained for rebase compatibility.
    pub fn hooks(&self) -> Vec<&PluginManifest> {
        self.all_manifests()
    }

    pub fn all_manifests(&self) -> Vec<&PluginManifest> {
        self.manifests.values().collect()
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn tools(&self) -> &[PluginToolManifest] {
        &self.manifest_tools
    }

    pub fn has_provider(&self, name: &str) -> bool {
        self.manifest_providers.contains(name)
    }

    pub fn tool_module_path(&self, tool: &str) -> Option<&str> {
        self.tool_modules.get(tool).map(String::as_str)
    }

    pub fn provider_module_path(&self, provider: &str) -> Option<&str> {
        self.provider_modules.get(provider).map(String::as_str)
    }

    fn rebuild_indexes(&mut self) {
        self.manifest_tools.clear();
        self.manifest_providers.clear();
        self.tool_modules.clear();
        self.provider_modules.clear();

        for manifest in self.manifests.values() {
            let module_path = manifest.module_path.clone();
            self.manifest_tools.extend(manifest.tools.iter().cloned());
            for tool in &manifest.tools {
                self.tool_modules
                    .entry(tool.name.clone())
                    .or_insert_with(|| module_path.clone());
            }
            for provider in &manifest.providers {
                let provider = provider.trim().to_string();
                self.manifest_providers.insert(provider.clone());
                self.provider_modules
                    .entry(provider)
                    .or_insert_with(|| module_path.clone());
            }
        }
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PluginRegistry {
    fn clone(&self) -> Self {
        Self {
            plugins: self.plugins.clone(),
            // Dynamic tool/hook handlers are not cloneable. Runtime registry clones only
            // need manifest-derived indexes for routing checks.
            tools: Vec::new(),
            hooks: Vec::new(),
            diagnostics: self.diagnostics.clone(),
            manifests: self.manifests.clone(),
            manifest_tools: self.manifest_tools.clone(),
            manifest_providers: self.manifest_providers.clone(),
            tool_modules: self.tool_modules.clone(),
            provider_modules: self.provider_modules.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(id: &str, tool_name: &str, provider: &str) -> PluginManifest {
        PluginManifest {
            id: id.to_string(),
            name: None,
            description: None,
            version: Some("1.0.0".to_string()),
            config_schema: None,
            capabilities: Vec::new(),
            module_path: "plugins/demo.wasm".to_string(),
            wit_packages: vec!["zeroclaw:tools@1.0.0".to_string()],
            tools: vec![PluginToolManifest {
                name: tool_name.to_string(),
                description: format!("{tool_name} description"),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            }],
            providers: vec![provider.to_string()],
        }
    }

    #[test]
    fn empty_registry() {
        let reg = PluginRegistry::new();
        assert_eq!(reg.active_count(), 0);
        assert!(reg.plugins.is_empty());
        assert!(reg.tools.is_empty());
        assert!(reg.tools().is_empty());
        assert!(reg.hooks.is_empty());
        assert!(reg.hooks().is_empty());
        assert!(reg.all_manifests().is_empty());
        assert!(!reg.has_provider("demo"));
        assert!(reg.diagnostics.is_empty());
    }

    #[test]
    fn active_count_filters_correctly() {
        let mut reg = PluginRegistry::new();
        reg.plugins.push(PluginRecord {
            id: "a".into(),
            name: None,
            version: None,
            description: None,
            source: "/tmp/a".into(),
            origin: PluginOrigin::Bundled,
            status: PluginStatus::Active,
        });
        reg.plugins.push(PluginRecord {
            id: "b".into(),
            name: None,
            version: None,
            description: None,
            source: "/tmp/b".into(),
            origin: PluginOrigin::Global,
            status: PluginStatus::Disabled,
        });
        reg.plugins.push(PluginRecord {
            id: "c".into(),
            name: None,
            version: None,
            description: None,
            source: "/tmp/c".into(),
            origin: PluginOrigin::Workspace,
            status: PluginStatus::Error("boom".into()),
        });
        assert_eq!(reg.active_count(), 1);
    }

    #[test]
    fn manifest_indexes_replace_on_reregister() {
        let mut reg = PluginRegistry::default();
        reg.register(manifest_with(
            "demo",
            "tool_v1",
            "provider_v1_for_replace_test",
        ));
        reg.register(manifest_with(
            "demo",
            "tool_v2",
            "provider_v2_for_replace_test",
        ));

        assert_eq!(reg.len(), 1);
        assert_eq!(reg.tools().len(), 1);
        assert_eq!(reg.tools()[0].name, "tool_v2");
        assert!(reg.has_provider("provider_v2_for_replace_test"));
        assert!(!reg.has_provider("provider_v1_for_replace_test"));
    }
}
