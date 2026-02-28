//! Plugin registry — collects loaded plugins, their tools, hooks, and diagnostics.
//!
//! Mirrors OpenClaw's `PluginRegistry` / `createPluginRegistry()`.

use crate::hooks::HookHandler;
use crate::tools::traits::Tool;

use super::manifest::PluginManifest;

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
#[derive(Debug)]
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
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            tools: Vec::new(),
            hooks: Vec::new(),
            diagnostics: Vec::new(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry() {
        let reg = PluginRegistry::new();
        assert_eq!(reg.active_count(), 0);
        assert!(reg.plugins.is_empty());
        assert!(reg.tools.is_empty());
        assert!(reg.hooks.is_empty());
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
}
