//! Plugin host: discovery, loading, lifecycle management.

use super::error::PluginError;
use super::signature::{self, SignatureMode, VerificationResult};
use super::{PluginCapability, PluginInfo, PluginManifest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Manages the lifecycle of WASM plugins.
pub struct PluginHost {
    plugins_dir: PathBuf,
    loaded: HashMap<String, LoadedPlugin>,
    signature_mode: SignatureMode,
    trusted_publisher_keys: Vec<String>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    wasm_path: PathBuf,
    #[allow(dead_code)]
    verification: VerificationResult,
}

impl PluginHost {
    /// Create a new plugin host with the given plugins directory.
    pub fn new(workspace_dir: &Path) -> Result<Self, PluginError> {
        Self::with_security(workspace_dir, SignatureMode::Disabled, Vec::new())
    }

    /// Create a new plugin host with signature verification settings.
    pub fn with_security(
        workspace_dir: &Path,
        signature_mode: SignatureMode,
        trusted_publisher_keys: Vec<String>,
    ) -> Result<Self, PluginError> {
        let plugins_dir = workspace_dir.join("plugins");
        if !plugins_dir.exists() {
            std::fs::create_dir_all(&plugins_dir)?;
        }

        let mut host = Self {
            plugins_dir,
            loaded: HashMap::new(),
            signature_mode,
            trusted_publisher_keys,
        };

        host.discover()?;
        Ok(host)
    }

    /// Parse the signature mode string from config into a `SignatureMode`.
    pub fn parse_signature_mode(mode: &str) -> SignatureMode {
        match mode.to_lowercase().as_str() {
            "strict" => SignatureMode::Strict,
            "permissive" => SignatureMode::Permissive,
            _ => SignatureMode::Disabled,
        }
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
                if manifest_path.exists()
                    && let Ok(manifest) = self.load_manifest(&manifest_path)
                {
                    // Verify plugin signature
                    let manifest_toml = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                    match self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest) {
                        Ok(verification) => {
                            let wasm_path = path.join(&manifest.wasm_path);
                            self.loaded.insert(
                                manifest.name.clone(),
                                LoadedPlugin {
                                    manifest,
                                    wasm_path,
                                    verification,
                                },
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                plugin = path.display().to_string(),
                                error = %e,
                                "skipping plugin due to signature verification failure"
                            );
                        }
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

    /// Verify a plugin's signature against configured policy.
    fn verify_plugin_signature(
        &self,
        name: &str,
        manifest_toml: &str,
        manifest: &PluginManifest,
    ) -> Result<VerificationResult, PluginError> {
        signature::enforce_signature_policy(
            name,
            manifest_toml,
            manifest.signature.as_deref(),
            manifest.publisher_key.as_deref(),
            &self.trusted_publisher_keys,
            self.signature_mode,
        )
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

        // Verify plugin signature before installing
        let manifest_toml = std::fs::read_to_string(&manifest_path)?;
        let verification =
            self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest)?;

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
                verification,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_empty_plugin_dir() {
        let dir = tempdir().unwrap();
        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn test_discover_with_manifest() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("test-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "test-plugin"
version = "0.1.0"
description = "A test plugin"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = []
"#,
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let plugins = host.list_plugins();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "test-plugin");
    }

    #[test]
    fn test_tool_plugins_filter() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // Tool plugin
        let tool_dir = plugins_base.join("my-tool");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(
            tool_dir.join("manifest.toml"),
            r#"
name = "my-tool"
version = "0.1.0"
wasm_path = "tool.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        // Channel plugin
        let chan_dir = plugins_base.join("my-channel");
        std::fs::create_dir_all(&chan_dir).unwrap();
        std::fs::write(
            chan_dir.join("manifest.toml"),
            r#"
name = "my-channel"
version = "0.1.0"
wasm_path = "channel.wasm"
capabilities = ["channel"]
"#,
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert_eq!(host.list_plugins().len(), 2);
        assert_eq!(host.tool_plugins().len(), 1);
        assert_eq!(host.channel_plugins().len(), 1);
        assert_eq!(host.tool_plugins()[0].name, "my-tool");
    }

    #[test]
    fn test_get_plugin() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("lookup-test");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "lookup-test"
version = "1.0.0"
description = "Lookup test"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.get_plugin("lookup-test").is_some());
        assert!(host.get_plugin("nonexistent").is_none());
    }

    #[test]
    fn test_remove_plugin() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("removable");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "removable"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        let mut host = PluginHost::new(dir.path()).unwrap();
        assert_eq!(host.list_plugins().len(), 1);

        host.remove("removable").unwrap();
        assert!(host.list_plugins().is_empty());
        assert!(!plugin_dir.exists());
    }

    #[test]
    fn test_remove_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let mut host = PluginHost::new(dir.path()).unwrap();
        assert!(host.remove("ghost").is_err());
    }
}
