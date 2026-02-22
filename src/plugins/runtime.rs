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
        for dir in &config.dirs {
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

pub fn initialize_from_config(config: &PluginsConfig) -> Result<()> {
    let runtime = PluginRuntime::new();
    let registry = runtime.load_registry_from_config(config)?;
    let mut guard = registry_cell()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = registry;
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

    #[test]
    fn runtime_rejects_invalid_manifest() {
        let runtime = PluginRuntime::new();
        assert!(runtime.load_manifest(PluginManifest::default()).is_err());
    }

    #[test]
    fn runtime_loads_plugin_manifest_files() {
        let dir = TempDir::new().expect("temp dir");
        let manifest_path = dir.path().join("demo.plugin.toml");
        std::fs::write(
            &manifest_path,
            r#"
id = "demo"
version = "1.0.0"
module_path = "plugins/demo.wasm"
wit_packages = ["zeroclaw:tools@1.0.0"]

[[tools]]
name = "demo_tool"
description = "demo tool"

providers = ["demo-provider"]
"#,
        )
        .expect("write manifest");

        let runtime = PluginRuntime::new();
        let cfg = PluginsConfig {
            enabled: true,
            dirs: vec![dir.path().to_string_lossy().to_string()],
            ..PluginsConfig::default()
        };
        let reg = runtime
            .load_registry_from_config(&cfg)
            .expect("load registry");
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.tools().len(), 1);
        assert!(reg.has_provider("demo-provider"));
    }
}
