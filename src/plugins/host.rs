//! Plugin host: discovery, loading, lifecycle management.

use super::error::PluginError;
use super::signature::{self, SignatureMode, VerificationResult};
use super::{PluginCapability, PluginInfo, PluginManifest};
use sha2::{Digest, Sha256};
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
    /// SHA-256 hash of the WASM binary, hex-encoded.
    wasm_sha256: Option<String>,
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
                if manifest_path.exists() {
                    if let Ok(manifest) = self.load_manifest(&manifest_path) {
                        // Verify plugin signature
                        let manifest_toml =
                            std::fs::read_to_string(&manifest_path).unwrap_or_default();
                        match self.verify_plugin_signature(
                            &manifest.name,
                            &manifest_toml,
                            &manifest,
                        ) {
                            Ok(verification) => {
                                let wasm_path = path.join(&manifest.wasm_path);
                                let wasm_sha256 = if wasm_path.exists() {
                                    Self::compute_wasm_hash(&wasm_path).ok()
                                } else {
                                    None
                                };
                                self.loaded.insert(
                                    manifest.name.clone(),
                                    LoadedPlugin {
                                        manifest,
                                        wasm_path,
                                        verification,
                                        wasm_sha256,
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
        }

        Ok(())
    }

    fn load_manifest(&self, path: &Path) -> Result<PluginManifest, PluginError> {
        let content = std::fs::read_to_string(path)?;
        PluginManifest::parse(&content)
    }

    /// Compute the SHA-256 hash of a WASM binary file, returning a hex-encoded string.
    fn compute_wasm_hash(wasm_path: &Path) -> Result<String, PluginError> {
        let bytes = std::fs::read(wasm_path)?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hash = hasher.finalize();
        Ok(hex::encode(hash))
    }

    /// Verify that a plugin's WASM binary matches the stored hash.
    /// Returns an error if the hash doesn't match.
    pub fn verify_wasm_integrity(&self, name: &str) -> Result<(), PluginError> {
        let plugin = self
            .loaded
            .get(name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        if let Some(expected) = &plugin.wasm_sha256 {
            let actual = Self::compute_wasm_hash(&plugin.wasm_path)?;
            if &actual != expected {
                return Err(PluginError::HashMismatch {
                    plugin: name.to_string(),
                    expected: expected.clone(),
                    actual,
                });
            }
        }

        Ok(())
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
                wasm_sha256: p.wasm_sha256.clone(),
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
            wasm_sha256: p.wasm_sha256.clone(),
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

        let wasm_sha256 = Self::compute_wasm_hash(&wasm_dest).ok();
        self.loaded.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
                wasm_path: wasm_dest,
                verification,
                wasm_sha256,
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

    /// Get tool-capable plugins with their resolved plugin directories.
    pub fn tool_plugins(&self) -> Vec<(&PluginManifest, PathBuf)> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Tool))
            .map(|p| {
                let plugin_dir = p
                    .wasm_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf();
                (&p.manifest, plugin_dir)
            })
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
    use crate::plugins::{PluginCapability, PluginPermission};
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
        assert_eq!(host.tool_plugins()[0].0.name, "my-tool");
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

    #[test]
    fn test_discover_reads_multiple_plugin_dirs_and_parses_manifests() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // Plugin A — tool with full manifest fields
        let plugin_a = plugins_base.join("alpha");
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::write(
            plugin_a.join("manifest.toml"),
            r#"
name = "alpha"
version = "1.2.3"
description = "Alpha tool plugin"
wasm_path = "alpha.wasm"
capabilities = ["tool"]
permissions = ["http_client", "file_read"]
"#,
        )
        .unwrap();

        // Plugin B — channel with minimal fields
        let plugin_b = plugins_base.join("beta");
        std::fs::create_dir_all(&plugin_b).unwrap();
        std::fs::write(
            plugin_b.join("manifest.toml"),
            r#"
name = "beta"
version = "0.0.1"
wasm_path = "beta.wasm"
capabilities = ["channel"]
"#,
        )
        .unwrap();

        // Plugin C — multiple capabilities
        let plugin_c = plugins_base.join("gamma");
        std::fs::create_dir_all(&plugin_c).unwrap();
        std::fs::write(
            plugin_c.join("manifest.toml"),
            r#"
name = "gamma"
version = "2.0.0"
description = "Multi-capability plugin"
wasm_path = "gamma.wasm"
capabilities = ["tool", "channel"]
permissions = ["memory_read", "memory_write"]
"#,
        )
        .unwrap();

        // Non-plugin directory (no manifest.toml) — should be skipped
        let no_manifest_dir = plugins_base.join("no-manifest");
        std::fs::create_dir_all(&no_manifest_dir).unwrap();
        std::fs::write(no_manifest_dir.join("README.md"), "not a plugin").unwrap();

        // Stray file in plugins dir — should be skipped
        std::fs::write(plugins_base.join("stray.txt"), "ignored").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let plugins = host.list_plugins();

        // All three valid plugins discovered, non-plugin entries skipped
        assert_eq!(plugins.len(), 3);

        // Verify each plugin was parsed correctly
        let alpha = host.get_plugin("alpha").expect("alpha should be discovered");
        assert_eq!(alpha.version, "1.2.3");
        assert_eq!(alpha.description.as_deref(), Some("Alpha tool plugin"));
        assert_eq!(alpha.capabilities, vec![PluginCapability::Tool]);
        assert_eq!(
            alpha.permissions,
            vec![PluginPermission::HttpClient, PluginPermission::FileRead]
        );
        assert!(alpha.wasm_path.ends_with("alpha.wasm"));

        let beta = host.get_plugin("beta").expect("beta should be discovered");
        assert_eq!(beta.version, "0.0.1");
        assert!(beta.description.is_none());
        assert_eq!(beta.capabilities, vec![PluginCapability::Channel]);
        assert!(beta.permissions.is_empty());

        let gamma = host.get_plugin("gamma").expect("gamma should be discovered");
        assert_eq!(gamma.version, "2.0.0");
        assert_eq!(
            gamma.capabilities,
            vec![PluginCapability::Tool, PluginCapability::Channel]
        );
        assert_eq!(
            gamma.permissions,
            vec![PluginPermission::MemoryRead, PluginPermission::MemoryWrite]
        );
    }

    #[test]
    fn test_hash_recalculated_on_rediscovery() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("hash-reload");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "hash-reload"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        // Write initial WASM content
        std::fs::write(plugin_dir.join("plugin.wasm"), b"original-wasm-bytes").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let info = host.get_plugin("hash-reload").expect("plugin should exist");
        let original_hash = info.wasm_sha256.clone().expect("hash should be recorded");

        // Replace WASM binary with different content (simulates a plugin update)
        std::fs::write(plugin_dir.join("plugin.wasm"), b"updated-wasm-bytes-v2").unwrap();

        // Re-discover by creating a new host (simulates reload)
        let host2 = PluginHost::new(dir.path()).unwrap();
        let info2 = host2.get_plugin("hash-reload").expect("plugin should exist after reload");
        let new_hash = info2.wasm_sha256.clone().expect("hash should be recorded after reload");

        assert_ne!(original_hash, new_hash, "hash must change when WASM binary changes");

        // Verify the new hash matches the actual file content
        let expected = PluginHost::compute_wasm_hash(&plugin_dir.join("plugin.wasm")).unwrap();
        assert_eq!(new_hash, expected, "hash should match freshly computed value");
    }

    #[test]
    fn test_hash_recalculated_on_reinstall() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins_base).unwrap();

        // Create a source directory for install
        let source_dir = dir.path().join("source");
        std::fs::create_dir_all(&source_dir).unwrap();

        std::fs::write(
            source_dir.join("manifest.toml"),
            r#"
name = "reinstallable"
version = "1.0.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();
        std::fs::write(source_dir.join("plugin.wasm"), b"version-one").unwrap();

        // Install the plugin
        let mut host = PluginHost::new(dir.path()).unwrap();
        host.install(source_dir.to_str().unwrap()).unwrap();
        let hash_v1 = host
            .get_plugin("reinstallable")
            .unwrap()
            .wasm_sha256
            .clone()
            .expect("hash should be set after install");

        // Remove the plugin
        host.remove("reinstallable").unwrap();

        // Update source with new WASM content and reinstall
        std::fs::write(source_dir.join("plugin.wasm"), b"version-two").unwrap();
        host.install(source_dir.to_str().unwrap()).unwrap();
        let hash_v2 = host
            .get_plugin("reinstallable")
            .unwrap()
            .wasm_sha256
            .clone()
            .expect("hash should be set after reinstall");

        assert_ne!(hash_v1, hash_v2, "hash must differ after reinstall with new binary");

        // Verify v2 hash is correct
        let installed_wasm = plugins_base.join("reinstallable").join("plugin.wasm");
        let expected = PluginHost::compute_wasm_hash(&installed_wasm).unwrap();
        assert_eq!(hash_v2, expected, "reinstall hash should match computed value");
    }

    #[test]
    fn test_mismatched_hash_produces_clear_error() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("tampered");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "tampered"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        // Write original WASM binary so the host records its SHA-256 hash.
        std::fs::write(plugin_dir.join("plugin.wasm"), b"original-wasm-content").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();

        // Sanity: hash was recorded and integrity passes before tampering.
        let info = host.get_plugin("tampered").expect("plugin should be discovered");
        assert!(info.wasm_sha256.is_some(), "hash must be recorded at discovery time");
        host.verify_wasm_integrity("tampered")
            .expect("integrity check should pass on unmodified binary");

        // Tamper with the WASM binary on disk.
        std::fs::write(plugin_dir.join("plugin.wasm"), b"malicious-replacement").unwrap();

        // verify_wasm_integrity must fail with a clear error.
        let err = host
            .verify_wasm_integrity("tampered")
            .expect_err("integrity check must fail after tampering");

        let msg = err.to_string();
        assert!(msg.contains("integrity check failed"), "error should mention integrity failure: {msg}");
        assert!(msg.contains("tampered"), "error should name the plugin: {msg}");
        assert!(msg.contains("expected hash"), "error should include expected hash: {msg}");
        assert!(msg.contains("got"), "error should include actual hash: {msg}");
    }

    #[test]
    fn test_missing_wasm_at_discovery_means_no_hash_no_mismatch() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("no-wasm");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "no-wasm"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();
        // Deliberately do NOT create plugin.wasm

        let host = PluginHost::new(dir.path()).unwrap();
        let info = host.get_plugin("no-wasm").expect("plugin should be discovered");
        assert!(info.wasm_sha256.is_none(), "no hash when WASM file is absent");

        // No hash means no mismatch — verify passes.
        host.verify_wasm_integrity("no-wasm")
            .expect("no hash means no mismatch");
    }

    #[test]
    fn test_discover_skips_dirs_with_invalid_manifest() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // Valid plugin
        let valid_dir = plugins_base.join("valid");
        std::fs::create_dir_all(&valid_dir).unwrap();
        std::fs::write(
            valid_dir.join("manifest.toml"),
            r#"
name = "valid"
version = "1.0.0"
wasm_path = "valid.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        // Plugin with broken TOML — should be skipped silently
        let broken_dir = plugins_base.join("broken");
        std::fs::create_dir_all(&broken_dir).unwrap();
        std::fs::write(
            broken_dir.join("manifest.toml"),
            "this is not valid toml {{{",
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let plugins = host.list_plugins();

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "valid");
    }
}
