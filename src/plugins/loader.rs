//! Plugin loader — takes discovered plugins, runs registration, builds the registry.
//!
//! Mirrors OpenClaw's `loader.ts`: iterates discovered plugins, resolves
//! enable/disable state from config, calls `Plugin::register()` with a
//! `PluginApi`, and collects tools/hooks/diagnostics into a `PluginRegistry`.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use tracing::{info, warn};

use crate::config::PluginsConfig;

use super::discovery::discover_plugins;
use super::registry::*;
use super::traits::{Plugin, PluginApi, PluginLogger};

/// Resolve whether a discovered plugin should be enabled.
fn resolve_enable(id: &str, cfg: &PluginsConfig) -> Result<(), String> {
    if !cfg.enabled {
        return Err("plugins disabled".into());
    }
    if cfg.deny.iter().any(|d| d == id) {
        return Err("blocked by denylist".into());
    }
    if !cfg.allow.is_empty() && !cfg.allow.iter().any(|a| a == id) {
        return Err("not in allowlist".into());
    }
    if let Some(entry) = cfg.entries.get(id) {
        if entry.enabled == Some(false) {
            return Err("disabled in config".into());
        }
    }
    Ok(())
}

/// Run `plugin.register(api)` with panic isolation.
///
/// Returns `Ok(api)` on success, `Err(message)` if the plugin panicked or
/// returned an error — matching OpenClaw's try/catch isolation pattern.
fn run_register(
    plugin: &dyn Plugin,
    plugin_id: &str,
    plugin_config: serde_json::Value,
) -> Result<PluginApi, String> {
    let mut api = PluginApi {
        plugin_id: plugin_id.to_string(),
        tools: Vec::new(),
        hooks: Vec::new(),
        config: plugin_config,
        logger: PluginLogger::new(plugin_id),
    };

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| plugin.register(&mut api)));

    match result {
        Ok(Ok(())) => Ok(api),
        Ok(Err(e)) => Err(format!("register() returned error: {e}")),
        Err(_) => Err("register() panicked".into()),
    }
}

/// Load all plugins: discover → filter → register → collect into registry.
///
/// `builtin_plugins` are compiled-in plugins (like OpenClaw's bundled extensions).
/// They are registered first, then discovered plugins from disk.
pub fn load_plugins(
    cfg: &PluginsConfig,
    workspace_dir: Option<&std::path::Path>,
    builtin_plugins: Vec<Box<dyn Plugin>>,
) -> PluginRegistry {
    let mut registry = PluginRegistry::new();

    if !cfg.enabled {
        registry.push_diagnostic(PluginDiagnostic {
            level: DiagnosticLevel::Info,
            plugin_id: None,
            source: None,
            message: "plugin system disabled".into(),
        });
        return registry;
    }

    let mut loaded_ids = HashSet::new();

    // 1. Builtin plugins (compiled-in, always available)
    for plugin in builtin_plugins {
        let manifest = plugin.manifest().clone();
        let id = manifest.id.clone();

        match resolve_enable(&id, cfg) {
            Err(reason) => {
                info!(plugin = %id, reason = %reason, "plugin disabled");
                registry.plugins.push(PluginRecord {
                    id,
                    name: manifest.name,
                    version: manifest.version,
                    description: manifest.description,
                    source: "(builtin)".into(),
                    origin: PluginOrigin::Bundled,
                    status: PluginStatus::Disabled,
                });
            }
            Ok(()) => {
                let plugin_config = cfg
                    .entries
                    .get(&id)
                    .map(|e| e.config.clone())
                    .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

                match run_register(plugin.as_ref(), &id, plugin_config) {
                    Ok(api) => {
                        let tool_count = api.tools.len();
                        let hook_count = api.hooks.len();
                        for tool in api.tools {
                            registry.tools.push(PluginToolRegistration {
                                plugin_id: id.clone(),
                                tool,
                            });
                        }
                        for handler in api.hooks {
                            registry.hooks.push(PluginHookRegistration {
                                plugin_id: id.clone(),
                                handler,
                            });
                        }
                        info!(
                            plugin = %id,
                            tools = tool_count,
                            hooks = hook_count,
                            "plugin registered"
                        );
                        registry.plugins.push(PluginRecord {
                            id: id.clone(),
                            name: manifest.name,
                            version: manifest.version,
                            description: manifest.description,
                            source: "(builtin)".into(),
                            origin: PluginOrigin::Bundled,
                            status: PluginStatus::Active,
                        });
                        loaded_ids.insert(id);
                    }
                    Err(err) => {
                        warn!(plugin = %id, error = %err, "plugin registration failed");
                        registry.push_diagnostic(PluginDiagnostic {
                            level: DiagnosticLevel::Error,
                            plugin_id: Some(id.clone()),
                            source: Some("(builtin)".into()),
                            message: err.clone(),
                        });
                        registry.plugins.push(PluginRecord {
                            id,
                            name: manifest.name,
                            version: manifest.version,
                            description: manifest.description,
                            source: "(builtin)".into(),
                            origin: PluginOrigin::Bundled,
                            status: PluginStatus::Error(err),
                        });
                    }
                }
            }
        }
    }

    // 2. Discovered plugins from disk
    let extra_paths: Vec<PathBuf> = cfg
        .load_paths
        .iter()
        .map(|p| PathBuf::from(shellexpand::tilde(p).as_ref()))
        .collect();

    let discovery = discover_plugins(workspace_dir, &extra_paths);
    registry.diagnostics.extend(discovery.diagnostics);

    for discovered in discovery.plugins {
        let id = discovered.manifest.id.clone();

        // Skip if already loaded as builtin
        if loaded_ids.contains(&id) {
            registry.push_diagnostic(PluginDiagnostic {
                level: DiagnosticLevel::Info,
                plugin_id: Some(id.clone()),
                source: Some(discovered.dir.display().to_string()),
                message: "skipped: already loaded as builtin".into(),
            });
            continue;
        }

        match resolve_enable(&id, cfg) {
            Err(reason) => {
                info!(plugin = %id, reason = %reason, "plugin disabled");
                registry.plugins.push(PluginRecord {
                    id,
                    name: discovered.manifest.name,
                    version: discovered.manifest.version,
                    description: discovered.manifest.description,
                    source: discovered.dir.display().to_string(),
                    origin: discovered.origin,
                    status: PluginStatus::Disabled,
                });
            }
            Ok(()) => {
                // Disk-discovered plugins are manifest-only for now.
                // Dynamic loading (libloading / WASM) is a future extension point.
                warn!(
                    plugin = %id,
                    path = %discovered.dir.display(),
                    "discovered plugin has no compiled entry point; \
                     register as builtin or wait for dynamic loading support"
                );
                registry.plugins.push(PluginRecord {
                    id: id.clone(),
                    name: discovered.manifest.name,
                    version: discovered.manifest.version,
                    description: discovered.manifest.description,
                    source: discovered.dir.display().to_string(),
                    origin: discovered.origin,
                    status: PluginStatus::Error(
                        "dynamic loading not yet supported; register as builtin".into(),
                    ),
                });
                loaded_ids.insert(id);
            }
        }
    }

    let active = registry.active_count();
    let total = registry.plugins.len();
    info!(active, total, "plugin loading complete");

    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PluginsConfig;
    use crate::plugins::manifest::PluginManifest;
    use crate::plugins::traits::{Plugin, PluginApi};
    use async_trait::async_trait;

    struct OkPlugin {
        manifest: PluginManifest,
    }

    impl Plugin for OkPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn register(&self, _api: &mut PluginApi) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct PanicPlugin {
        manifest: PluginManifest,
    }

    impl Plugin for PanicPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn register(&self, _api: &mut PluginApi) -> anyhow::Result<()> {
            panic!("intentional panic");
        }
    }

    struct ErrorPlugin {
        manifest: PluginManifest,
    }

    impl Plugin for ErrorPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn register(&self, _api: &mut PluginApi) -> anyhow::Result<()> {
            anyhow::bail!("intentional error")
        }
    }

    fn make_manifest(id: &str) -> PluginManifest {
        PluginManifest {
            id: id.into(),
            name: Some(id.into()),
            version: Some("0.1.0".into()),
            description: None,
            config_schema: None,
        }
    }

    fn enabled_cfg() -> PluginsConfig {
        PluginsConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_system_returns_empty_registry() {
        let cfg = PluginsConfig {
            enabled: false,
            ..Default::default()
        };
        let reg = load_plugins(&cfg, None, vec![]);
        assert_eq!(reg.active_count(), 0);
        assert!(reg.diagnostics.iter().any(|d| d.message.contains("disabled")));
    }

    #[test]
    fn ok_plugin_is_active() {
        let cfg = enabled_cfg();
        let plugin: Box<dyn Plugin> = Box::new(OkPlugin {
            manifest: make_manifest("ok"),
        });
        let reg = load_plugins(&cfg, None, vec![plugin]);
        assert_eq!(reg.active_count(), 1);
        assert_eq!(reg.plugins[0].status, PluginStatus::Active);
    }

    #[test]
    fn panic_plugin_is_isolated() {
        let cfg = enabled_cfg();
        let plugin: Box<dyn Plugin> = Box::new(PanicPlugin {
            manifest: make_manifest("panicky"),
        });
        let reg = load_plugins(&cfg, None, vec![plugin]);
        assert_eq!(reg.active_count(), 0);
        match &reg.plugins[0].status {
            PluginStatus::Error(msg) => assert!(msg.contains("panic")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn error_plugin_is_isolated() {
        let cfg = enabled_cfg();
        let plugin: Box<dyn Plugin> = Box::new(ErrorPlugin {
            manifest: make_manifest("erroring"),
        });
        let reg = load_plugins(&cfg, None, vec![plugin]);
        assert_eq!(reg.active_count(), 0);
        match &reg.plugins[0].status {
            PluginStatus::Error(msg) => assert!(msg.contains("error")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn denylist_disables_plugin() {
        let cfg = PluginsConfig {
            enabled: true,
            deny: vec!["blocked".into()],
            ..Default::default()
        };
        let plugin: Box<dyn Plugin> = Box::new(OkPlugin {
            manifest: make_manifest("blocked"),
        });
        let reg = load_plugins(&cfg, None, vec![plugin]);
        assert_eq!(reg.active_count(), 0);
        assert_eq!(reg.plugins[0].status, PluginStatus::Disabled);
    }

    #[test]
    fn allowlist_filters_plugins() {
        let cfg = PluginsConfig {
            enabled: true,
            allow: vec!["allowed".into()],
            ..Default::default()
        };
        let allowed: Box<dyn Plugin> = Box::new(OkPlugin {
            manifest: make_manifest("allowed"),
        });
        let blocked: Box<dyn Plugin> = Box::new(OkPlugin {
            manifest: make_manifest("not-allowed"),
        });
        let reg = load_plugins(&cfg, None, vec![allowed, blocked]);
        assert_eq!(reg.active_count(), 1);
        assert_eq!(reg.plugins[0].id, "allowed");
        assert_eq!(reg.plugins[0].status, PluginStatus::Active);
        assert_eq!(reg.plugins[1].status, PluginStatus::Disabled);
    }
}
