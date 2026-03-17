//! Plugin host: discovery, loading, lifecycle management.

use super::error::PluginError;
use super::{PluginCapability, PluginInfo, PluginManifest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Manages the lifecycle of WASM plugins.
pub struct PluginHost {
    plugins_dir: PathBuf,
    loaded: HashMap<String, LoadedPlugin>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    wasm_path: PathBuf,
}

impl PluginHost {
    /// Create a new plugin host with the given plugins directory.
    pub fn new(workspace_dir: &Path) -> Result<Self, PluginError> {
        let plugins_dir = workspace_dir.join("plugins");
        if !plugins_dir.exists() {
            std::fs::create_dir_all(&plugins_dir)?;
        }

        let mut host = Self {
            plugins_dir,
            loaded: HashMap::new(),
        };

        host.discover()?;
        Ok(host)
    }

    /// Discover plugins in the plugins directory.
    fn discover(&mut self) -> Result<(), PluginError> {
        if !self.plugins_dir.exists() {
            return Ok(());
        }

        let entries = std::fs::read_dir(&self.plugins_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let manifest_path = path.join("manifest.toml");
                if manifest_path.exists() {
                    if let Ok(manifest) = self.load_manifest(&manifest_path) {
                        let wasm_path = path.join(&manifest.wasm_path);
                        self.loaded.insert(
                            manifest.name.clone(),
                            LoadedPlugin {
                                manifest,
                                wasm_path,
                            },
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn load_manifest(&self, path: &Path) -> Result<PluginManifest, PluginError> {
        let content = std::fs::read_to_string(path)?;
        let manifest: PluginManifest = toml::from_str(&content)?;
        Ok(manifest)
    }

    /// List all discovered plugins.
    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        self.loaded
            .values()
            .map(|p| PluginInfo {
                name: p.manifest.name.clone(),
                version: p.manifest.version.clone(),
                description: p.manifest.description.clone(),
                capabilities: p.manifest.capabilities.clone(),
                permissions: p.manifest.permissions.clone(),
                wasm_path: p.wasm_path.clone(),
                loaded: p.wasm_path.exists(),
            })
            .collect()
    }

    /// Get info about a specific plugin.
    pub fn get_plugin(&self, name: &str) -> Option<PluginInfo> {
        self.loaded.get(name).map(|p| PluginInfo {
            name: p.manifest.name.clone(),
            version: p.manifest.version.clone(),
            description: p.manifest.description.clone(),
            capabilities: p.manifest.capabilities.clone(),
            permissions: p.manifest.permissions.clone(),
            wasm_path: p.wasm_path.clone(),
            loaded: p.wasm_path.exists(),
        })
    }

    /// Install a plugin from a directory path.
    pub fn install(&mut self, source: &str) -> Result<(), PluginError> {
        let source_path = PathBuf::from(source);
        let manifest_path = if source_path.is_dir() {
            source_path.join("manifest.toml")
        } else {
            source_path.clone()
        };

        if !manifest_path.exists() {
            return Err(PluginError::NotFound(format!(
                "manifest.toml not found at {}",
                manifest_path.display()
            )));
        }

        let manifest = self.load_manifest(&manifest_path)?;
        let source_dir = manifest_path
            .parent()
            .ok_or_else(|| PluginError::InvalidManifest("no parent directory".into()))?;

        let wasm_source = source_dir.join(&manifest.wasm_path);
        if !wasm_source.exists() {
            return Err(PluginError::NotFound(format!(
                "WASM file not found: {}",
                wasm_source.display()
            )));
        }

        if self.loaded.contains_key(&manifest.name) {
            return Err(PluginError::AlreadyLoaded(manifest.name));
        }

        // Copy plugin to plugins directory
        let dest_dir = self.plugins_dir.join(&manifest.name);
        std::fs::create_dir_all(&dest_dir)?;

        // Copy manifest
        std::fs::copy(&manifest_path, dest_dir.join("manifest.toml"))?;

        // Copy WASM file
        let wasm_dest = dest_dir.join(&manifest.wasm_path);
        if let Some(parent) = wasm_dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&wasm_source, &wasm_dest)?;

        self.loaded.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
                wasm_path: wasm_dest,
            },
        );

        Ok(())
    }

    /// Remove a plugin by name.
    pub fn remove(&mut self, name: &str) -> Result<(), PluginError> {
        if self.loaded.remove(name).is_none() {
            return Err(PluginError::NotFound(name.to_string()));
        }

        let plugin_dir = self.plugins_dir.join(name);
        if plugin_dir.exists() {
            std::fs::remove_dir_all(plugin_dir)?;
        }

        Ok(())
    }

    /// Get tool-capable plugins.
    pub fn tool_plugins(&self) -> Vec<&PluginManifest> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Tool))
            .map(|p| &p.manifest)
            .collect()
    }

    /// Get channel-capable plugins.
    pub fn channel_plugins(&self) -> Vec<&PluginManifest> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Channel))
            .map(|p| &p.manifest)
            .collect()
    }

    /// Returns the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }
}
