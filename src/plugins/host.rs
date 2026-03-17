//! Plugin host: discovery, loading, lifecycle management.

use super::error::PluginError;
use super::PluginInfo;
use std::path::{Path, PathBuf};

/// Manages the lifecycle of WASM plugins.
pub struct PluginHost {
    plugins_dir: PathBuf,
}

impl PluginHost {
    /// Create a new plugin host with the given workspace directory.
    pub fn new(workspace_dir: &Path) -> Result<Self, PluginError> {
        let plugins_dir = workspace_dir.join("plugins");
        if !plugins_dir.exists() {
            std::fs::create_dir_all(&plugins_dir)?;
        }
        Ok(Self { plugins_dir })
    }

    /// List all discovered plugins.
    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        Vec::new()
    }

    /// Get info about a specific plugin.
    pub fn get_plugin(&self, _name: &str) -> Option<PluginInfo> {
        None
    }

    /// Install a plugin from a directory path.
    pub fn install(&mut self, _source: &str) -> Result<(), PluginError> {
        Err(PluginError::LoadFailed(
            "plugin host not yet fully implemented".into(),
        ))
    }

    /// Remove a plugin by name.
    pub fn remove(&mut self, name: &str) -> Result<(), PluginError> {
        Err(PluginError::NotFound(name.to_string()))
    }

    /// Returns the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }
}
