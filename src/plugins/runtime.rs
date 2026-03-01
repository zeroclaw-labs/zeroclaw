use anyhow::{Context, Result};
use std::path::Path;
use std::sync::{OnceLock, RwLock};

use super::manifest::PluginManifest;
use super::registry::PluginRegistry;
use crate::config::PluginsConfig;

#[derive(Debug, Default)]
pub struct PluginRuntime;

impl PluginRuntime {
    pub fn new() -> Self {
        Self
    }

    pub fn load_manifest(&self, manifest: PluginManifest) -> Result<PluginManifest> {
        if !manifest.is_valid() {
            anyhow::bail!("invalid plugin manifest")
        }
        Ok(manifest)
    }

    pub fn load_registry_from_config(&self, config: &PluginsConfig) -> Result<PluginRegistry> {
        let mut registry = PluginRegistry::default();
        if !config.enabled {
            return Ok(registry);
        }
        for dir in &config.load_paths {
            let path = Path::new(dir);
            if !path.exists() {
                continue;
            }
            let entries = std::fs::read_dir(path)
                .with_context(|| format!("failed to read plugin directory {}", path.display()))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let file_name = path
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or("");
                if !(file_name.ends_with(".plugin.toml") || file_name.ends_with(".plugin.json")) {
                    continue;
                }
                let raw = std::fs::read_to_string(&path).with_context(|| {
                    format!("failed to read plugin manifest {}", path.display())
                })?;
                let manifest: PluginManifest = if file_name.ends_with(".plugin.toml") {
                    toml::from_str(&raw).with_context(|| {
                        format!("failed to parse plugin TOML manifest {}", path.display())
                    })?
                } else {
                    serde_json::from_str(&raw).with_context(|| {
                        format!("failed to parse plugin JSON manifest {}", path.display())
                    })?
                };
                let manifest = self.load_manifest(manifest)?;
                registry.register(manifest);
            }
        }
        Ok(registry)
    }
}

fn registry_cell() -> &'static RwLock<PluginRegistry> {
    static CELL: OnceLock<RwLock<PluginRegistry>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(PluginRegistry::default()))
}

fn init_fingerprint_cell() -> &'static RwLock<Option<String>> {
    static CELL: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(None))
}

fn config_fingerprint(config: &PluginsConfig) -> String {
    serde_json::to_string(config).unwrap_or_else(|_| "<serialize-error>".to_string())
}

pub fn initialize_from_config(config: &PluginsConfig) -> Result<()> {
    let fingerprint = config_fingerprint(config);
    {
        let guard = init_fingerprint_cell()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.as_ref() == Some(&fingerprint) {
            tracing::debug!(
                "plugin registry already initialized for this config, skipping re-init"
            );
            return Ok(());
        }
    }

    let runtime = PluginRuntime::new();
    let registry = runtime.load_registry_from_config(config)?;
    {
        let mut guard = registry_cell()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = registry;
    }
    {
        let mut guard = init_fingerprint_cell()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(fingerprint);
    }

    Ok(())
}

pub fn current_registry() -> PluginRegistry {
    registry_cell()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_manifest(dir: &std::path::Path, id: &str, provider: &str, tool: &str) {
        let manifest_path = dir.join(format!("{id}.plugin.toml"));
        std::fs::write(
            &manifest_path,
            format!(
                r#"
id = "{id}"
version = "1.0.0"
module_path = "plugins/{id}.wasm"
wit_packages = ["zeroclaw:tools@1.0.0"]
providers = ["{provider}"]

[[tools]]
name = "{tool}"
description = "{tool} description"
"#
            ),
        )
        .expect("write manifest");
    }

    #[test]
    fn runtime_rejects_invalid_manifest() {
        let runtime = PluginRuntime::new();
        assert!(runtime.load_manifest(PluginManifest::default()).is_err());
    }

    #[test]
    fn runtime_loads_plugin_manifest_files() {
        let dir = TempDir::new().expect("temp dir");
        write_manifest(dir.path(), "demo", "demo-provider", "demo_tool");

        let runtime = PluginRuntime::new();
        let cfg = PluginsConfig {
            enabled: true,
            load_paths: vec![dir.path().to_string_lossy().to_string()],
            ..PluginsConfig::default()
        };
        let reg = runtime
            .load_registry_from_config(&cfg)
            .expect("load registry");
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.tools().len(), 1);
        assert!(reg.has_provider("demo-provider"));
    }

    #[test]
    fn initialize_from_config_applies_updated_plugin_dirs() {
        let dir_a = TempDir::new().expect("temp dir a");
        let dir_b = TempDir::new().expect("temp dir b");
        write_manifest(
            dir_a.path(),
            "reload_a",
            "reload-provider-a-for-runtime-test",
            "reload_tool_a",
        );
        write_manifest(
            dir_b.path(),
            "reload_b",
            "reload-provider-b-for-runtime-test",
            "reload_tool_b",
        );

        let cfg_a = PluginsConfig {
            enabled: true,
            load_paths: vec![dir_a.path().to_string_lossy().to_string()],
            ..PluginsConfig::default()
        };
        initialize_from_config(&cfg_a).expect("first initialization should succeed");
        let reg_a = current_registry();
        assert!(reg_a.has_provider("reload-provider-a-for-runtime-test"));

        let cfg_b = PluginsConfig {
            enabled: true,
            load_paths: vec![dir_b.path().to_string_lossy().to_string()],
            ..PluginsConfig::default()
        };
        initialize_from_config(&cfg_b).expect("second initialization should succeed");
        let reg_b = current_registry();
        assert!(reg_b.has_provider("reload-provider-b-for-runtime-test"));
        assert!(!reg_b.has_provider("reload-provider-a-for-runtime-test"));
    }
}
