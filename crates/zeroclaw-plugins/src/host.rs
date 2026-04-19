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

/// Status of a single diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagStatus {
    Pass,
    Warn,
    Fail,
}

impl std::fmt::Display for DiagStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiagStatus::Pass => write!(f, "PASS"),
            DiagStatus::Warn => write!(f, "WARN"),
            DiagStatus::Fail => write!(f, "FAIL"),
        }
    }
}

/// A single diagnostic check result.
#[derive(Debug, Clone)]
pub struct DiagCheck {
    pub name: String,
    pub status: DiagStatus,
    pub message: String,
}

/// Diagnostic results for one plugin.
#[derive(Debug, Clone)]
pub struct PluginDiagnostic {
    pub plugin_name: String,
    pub checks: Vec<DiagCheck>,
}

impl PluginDiagnostic {
    /// Overall status: the worst status among all checks.
    pub fn overall(&self) -> DiagStatus {
        if self.checks.iter().any(|c| c.status == DiagStatus::Fail) {
            DiagStatus::Fail
        } else if self.checks.iter().any(|c| c.status == DiagStatus::Warn) {
            DiagStatus::Warn
        } else {
            DiagStatus::Pass
        }
    }
}

/// Summary of a plugin reload operation.
pub struct ReloadSummary {
    /// Total plugins loaded after reload.
    pub total: usize,
    /// Plugins that are newly loaded (not present before reload).
    pub loaded: Vec<String>,
    /// Plugins that were unloaded (present before but not after reload).
    pub unloaded: Vec<String>,
    /// Error messages for plugins that failed to load.
    pub failed: Vec<String>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    wasm_path: PathBuf,
    #[allow(dead_code)]
    verification: VerificationResult,
    /// SHA-256 hash of the WASM binary, hex-encoded.
    wasm_sha256: Option<String>,
    /// Whether this plugin is enabled (user-togglable).
    enabled: bool,
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
                let manifest_path = {
                    let m = path.join("manifest.toml");
                    if m.exists() {
                        m
                    } else {
                        path.join("plugin.toml")
                    }
                };
                if manifest_path.exists()
                    && let Ok(manifest) = self.load_manifest(&manifest_path)
                {
                    // Verify plugin signature
                    let manifest_toml = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                    match self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest) {
                        Ok(verification) => {
                            let wasm_path = path.join(&manifest.wasm_path);
                            // Prefer the stored hash from the .sha256 sidecar
                            // file (written at install time) so that tampering
                            // between restarts is caught.  Fall back to computing
                            // the hash live for backwards compat with pre-hash
                            // installs.
                            let sidecar = wasm_path.with_extension("wasm.sha256");
                            let wasm_sha256 = if sidecar.exists() {
                                std::fs::read_to_string(&sidecar)
                                    .ok()
                                    .map(|s| s.trim().to_string())
                            } else if wasm_path.exists() {
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
                                    enabled: true,
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
        toml::from_str(&content).map_err(|e| PluginError::InvalidManifest(e.to_string()))
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
        } else {
            tracing::warn!(
                plugin = %name,
                "no stored SHA-256 hash for plugin; skipping integrity check \
                 (pre-hash install or missing WASM file)"
            );
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
                tools: p.manifest.tools.clone(),
                wasm_path: p.wasm_path.clone(),
                loaded: p.wasm_path.exists(),
                enabled: p.enabled,
                wasm_sha256: p.wasm_sha256.clone(),
                allowed_hosts: p.manifest.allowed_hosts.clone(),
                allowed_paths: p.manifest.allowed_paths.clone(),
                config: p.manifest.config.clone(),
                host_capabilities: p.manifest.host_capabilities.clone(),
            })
            .collect()
    }

    /// Load a plugin by name, verifying WASM binary integrity first.
    ///
    /// Returns the plugin info only if the hash check passes.  This must
    /// be called on every plugin load to satisfy the integrity-on-load
    /// acceptance criterion (US-ZCL-16).
    pub fn load_plugin(&self, name: &str) -> Result<PluginInfo, PluginError> {
        let plugin = self
            .loaded
            .get(name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        // Verify WASM binary integrity before returning info for loading.
        self.verify_wasm_integrity(name)?;

        Ok(PluginInfo {
            name: plugin.manifest.name.clone(),
            version: plugin.manifest.version.clone(),
            description: plugin.manifest.description.clone(),
            capabilities: plugin.manifest.capabilities.clone(),
            permissions: plugin.manifest.permissions.clone(),
            tools: plugin.manifest.tools.clone(),
            wasm_path: plugin.wasm_path.clone(),
            loaded: plugin.wasm_path.exists(),
            enabled: plugin.enabled,
            wasm_sha256: plugin.wasm_sha256.clone(),
            allowed_hosts: plugin.manifest.allowed_hosts.clone(),
            allowed_paths: plugin.manifest.allowed_paths.clone(),
            config: plugin.manifest.config.clone(),
            host_capabilities: plugin.manifest.host_capabilities.clone(),
        })
    }

    /// Get info about a specific plugin.
    pub fn get_plugin(&self, name: &str) -> Option<PluginInfo> {
        self.loaded.get(name).map(|p| PluginInfo {
            name: p.manifest.name.clone(),
            version: p.manifest.version.clone(),
            description: p.manifest.description.clone(),
            capabilities: p.manifest.capabilities.clone(),
            permissions: p.manifest.permissions.clone(),
            tools: p.manifest.tools.clone(),
            wasm_path: p.wasm_path.clone(),
            loaded: p.wasm_path.exists(),
            enabled: p.enabled,
            wasm_sha256: p.wasm_sha256.clone(),
            allowed_hosts: p.manifest.allowed_hosts.clone(),
            allowed_paths: p.manifest.allowed_paths.clone(),
            config: p.manifest.config.clone(),
            host_capabilities: p.manifest.host_capabilities.clone(),
        })
    }

    /// Enable a previously disabled plugin.
    ///
    /// Returns `Ok(())` if the plugin was found and enabled, or an error if the
    /// plugin does not exist.
    pub fn enable_plugin(&mut self, name: &str) -> Result<(), PluginError> {
        let plugin = self
            .loaded
            .get_mut(name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;
        plugin.enabled = true;
        Ok(())
    }

    /// Disable a plugin so it is no longer active.
    ///
    /// Returns `Ok(())` if the plugin was found and disabled, or an error if the
    /// plugin does not exist.
    pub fn disable_plugin(&mut self, name: &str) -> Result<(), PluginError> {
        let plugin = self
            .loaded
            .get_mut(name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;
        plugin.enabled = false;
        Ok(())
    }

    /// Install a plugin from a local directory or manifest path.
    ///
    /// For HTTP URLs and archive files, use `install_from_source_dir()` after
    /// downloading/extracting externally.
    pub fn install(&mut self, source: &str) -> Result<(), PluginError> {
        let source_path = PathBuf::from(source);
        let manifest_path = if source_path.is_dir() {
            let m = source_path.join("manifest.toml");
            if m.exists() {
                m
            } else {
                source_path.join("plugin.toml")
            }
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

        self.install_from_source_dir(source_dir, &manifest_path, &manifest, true)
    }

    /// Install from an already-extracted source directory.
    ///
    /// When `auto_enable` is true the plugin is immediately enabled (local
    /// directory installs). Remote/archive installs should pass `false` so the
    /// operator can review the manifest and configure required keys first.
    pub fn install_from_source_dir(
        &mut self,
        source_dir: &Path,
        manifest_path: &Path,
        manifest: &super::PluginManifest,
        auto_enable: bool,
    ) -> Result<(), PluginError> {
        let wasm_source = source_dir.join(&manifest.wasm_path);
        if !wasm_source.exists() {
            return Err(PluginError::NotFound(format!(
                "WASM file not found: {}",
                wasm_source.display()
            )));
        }

        if self.loaded.contains_key(&manifest.name) {
            return Err(PluginError::AlreadyLoaded(manifest.name.clone()));
        }

        // Verify plugin signature before installing
        let manifest_toml = std::fs::read_to_string(manifest_path)?;
        let verification =
            self.verify_plugin_signature(&manifest.name, &manifest_toml, manifest)?;

        // Copy plugin to plugins directory
        let dest_dir = self.plugins_dir.join(&manifest.name);
        std::fs::create_dir_all(&dest_dir)?;

        // Copy manifest
        std::fs::copy(manifest_path, dest_dir.join("manifest.toml"))?;

        // Copy WASM file (preserving any subdirectory structure)
        let wasm_dest = dest_dir.join(&manifest.wasm_path);
        if let Some(parent) = wasm_dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&wasm_source, &wasm_dest)?;

        let wasm_sha256 = Self::compute_wasm_hash(&wasm_dest).ok();

        // Persist the hash to a .sha256 sidecar file so it survives process restarts.
        if let Some(ref hash) = wasm_sha256 {
            let hash_path = wasm_dest.with_extension("wasm.sha256");
            std::fs::write(&hash_path, hash)?;
        }

        self.loaded.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest: manifest.clone(),
                wasm_path: wasm_dest,
                verification,
                wasm_sha256,
                enabled: auto_enable,
            },
        );

        if !auto_enable {
            tracing::info!(
                plugin = %manifest.name,
                "plugin installed but NOT enabled — use `zeroclaw plugin enable {}` after configuration",
                manifest.name
            );
        }

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
                let plugin_dir = p.wasm_path.parent().unwrap_or(Path::new(".")).to_path_buf();
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

    /// Reload all plugins by re-scanning the plugins directory.
    ///
    /// Returns a `ReloadSummary` describing what changed.
    pub fn reload(&mut self) -> Result<ReloadSummary, PluginError> {
        let before_names: std::collections::HashSet<String> = self.loaded.keys().cloned().collect();

        self.loaded.clear();
        let discover_result = self.discover();

        let after_names: std::collections::HashSet<String> = self.loaded.keys().cloned().collect();

        let loaded = after_names.difference(&before_names).cloned().collect();
        let unloaded = before_names.difference(&after_names).cloned().collect();
        let total = after_names.len();

        // If discover itself failed, report it but still return the summary.
        let failed = match discover_result {
            Ok(()) => Vec::new(),
            Err(e) => vec![e.to_string()],
        };

        Ok(ReloadSummary {
            total,
            loaded,
            unloaded,
            failed,
        })
    }

    /// Run diagnostic checks on a single plugin directory.
    ///
    /// Checks: 1) manifest exists and parses, 2) WASM file exists and is
    /// readable, 3) required config keys are declared, 4) allowed_hosts /
    /// allowed_paths compatibility with security levels, 5) WASM binary hash
    /// matches the stored hash (if hash verification is available).
    pub fn diagnose_plugin(&self, plugin_dir: &Path) -> PluginDiagnostic {
        let plugin_name = plugin_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let mut checks = Vec::new();

        // Check 1: manifest exists and is parseable
        let manifest_path = plugin_dir.join("manifest.toml");
        let manifest = if !manifest_path.exists() {
            let alt_path = plugin_dir.join("plugin.toml");
            if !alt_path.exists() {
                checks.push(DiagCheck {
                    name: "manifest".into(),
                    status: DiagStatus::Fail,
                    message: "no manifest.toml or plugin.toml found".into(),
                });
                return PluginDiagnostic {
                    plugin_name,
                    checks,
                };
            }
            match self.load_manifest(&alt_path) {
                Ok(m) => {
                    checks.push(DiagCheck {
                        name: "manifest".into(),
                        status: DiagStatus::Pass,
                        message: "plugin.toml is valid".into(),
                    });
                    Some(m)
                }
                Err(e) => {
                    checks.push(DiagCheck {
                        name: "manifest".into(),
                        status: DiagStatus::Fail,
                        message: format!("invalid plugin.toml: {e}"),
                    });
                    None
                }
            }
        } else {
            match self.load_manifest(&manifest_path) {
                Ok(m) => {
                    checks.push(DiagCheck {
                        name: "manifest".into(),
                        status: DiagStatus::Pass,
                        message: "manifest.toml is valid".into(),
                    });
                    Some(m)
                }
                Err(e) => {
                    checks.push(DiagCheck {
                        name: "manifest".into(),
                        status: DiagStatus::Fail,
                        message: format!("invalid manifest.toml: {e}"),
                    });
                    None
                }
            }
        };

        if let Some(manifest) = &manifest {
            // Check 2: WASM file exists and is readable
            let wasm_path = plugin_dir.join(&manifest.wasm_path);
            if !wasm_path.exists() {
                checks.push(DiagCheck {
                    name: "wasm_file".into(),
                    status: DiagStatus::Fail,
                    message: format!("WASM file not found: {}", manifest.wasm_path),
                });
            } else if std::fs::metadata(&wasm_path).is_err() {
                checks.push(DiagCheck {
                    name: "wasm_file".into(),
                    status: DiagStatus::Fail,
                    message: format!("WASM file unreadable: {}", manifest.wasm_path),
                });
            } else {
                checks.push(DiagCheck {
                    name: "wasm_file".into(),
                    status: DiagStatus::Pass,
                    message: "WASM file exists and is readable".into(),
                });
            }

            // Check 3: required config keys
            let mut missing_keys = Vec::new();
            for (key, decl) in &manifest.config {
                if let serde_json::Value::Object(obj) = decl
                    && obj
                        .get("required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                {
                    missing_keys.push(key.clone());
                }
            }
            if missing_keys.is_empty() {
                checks.push(DiagCheck {
                    name: "config".into(),
                    status: DiagStatus::Pass,
                    message: "no required config keys, or all have defaults".into(),
                });
            } else {
                missing_keys.sort();
                checks.push(DiagCheck {
                    name: "config".into(),
                    status: DiagStatus::Warn,
                    message: format!(
                        "required config keys (ensure set at runtime): {}",
                        missing_keys.join(", ")
                    ),
                });
            }

            // Check 4: allowed_hosts/allowed_paths compatibility with security levels
            let has_wildcard_hosts = manifest.allowed_hosts.iter().any(|h| h.contains('*'));
            let forbidden_prefixes: &[&str] = &["/etc", "/var", "/usr", "/bin", "/sbin", "/root"];
            let has_forbidden_paths = manifest.allowed_paths.values().any(|p| {
                let expanded = if p.starts_with("~/") {
                    std::env::var("HOME")
                        .map(|h| format!("{}/{}", h, &p[2..]))
                        .unwrap_or_else(|_| p.clone())
                } else {
                    p.clone()
                };
                forbidden_prefixes
                    .iter()
                    .any(|forbidden| expanded.starts_with(forbidden))
            });

            if has_wildcard_hosts && has_forbidden_paths {
                checks.push(DiagCheck {
                    name: "capabilities".into(),
                    status: DiagStatus::Fail,
                    message: "declares wildcard hosts AND forbidden paths — incompatible with all security levels".into(),
                });
            } else if has_forbidden_paths {
                checks.push(DiagCheck {
                    name: "capabilities".into(),
                    status: DiagStatus::Fail,
                    message: "declares paths in forbidden areas — rejected at all security levels"
                        .to_string(),
                });
            } else if has_wildcard_hosts {
                checks.push(DiagCheck {
                    name: "capabilities".into(),
                    status: DiagStatus::Warn,
                    message: "declares wildcard hosts — may conflict with strict/paranoid security policy".into(),
                });
            } else {
                checks.push(DiagCheck {
                    name: "capabilities".into(),
                    status: DiagStatus::Pass,
                    message: "no capability conflicts detected".into(),
                });
            }

            // Check 5: WASM binary hash verification
            let wasm_path = plugin_dir.join(&manifest.wasm_path);
            if wasm_path.exists() {
                let sidecar = wasm_path.with_extension("wasm.sha256");
                if sidecar.exists() {
                    match std::fs::read_to_string(&sidecar) {
                        Ok(expected) => {
                            let expected = expected.trim().to_string();
                            match Self::compute_wasm_hash(&wasm_path) {
                                Ok(actual) if actual == expected => {
                                    checks.push(DiagCheck {
                                        name: "hash".into(),
                                        status: DiagStatus::Pass,
                                        message: "WASM hash matches stored hash".into(),
                                    });
                                }
                                Ok(actual) => {
                                    checks.push(DiagCheck {
                                        name: "hash".into(),
                                        status: DiagStatus::Fail,
                                        message: format!(
                                            "WASM hash mismatch — expected {}, got {}",
                                            &expected[..12.min(expected.len())],
                                            &actual[..12.min(actual.len())]
                                        ),
                                    });
                                }
                                Err(e) => {
                                    checks.push(DiagCheck {
                                        name: "hash".into(),
                                        status: DiagStatus::Fail,
                                        message: format!("failed to compute WASM hash: {e}"),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            checks.push(DiagCheck {
                                name: "hash".into(),
                                status: DiagStatus::Warn,
                                message: format!("could not read hash sidecar file: {e}"),
                            });
                        }
                    }
                } else {
                    checks.push(DiagCheck {
                        name: "hash".into(),
                        status: DiagStatus::Warn,
                        message: "no stored hash — plugin may predate hash verification".into(),
                    });
                }
            }
            // If WASM file doesn't exist, check 2 already reported it.
        }

        PluginDiagnostic {
            plugin_name,
            checks,
        }
    }

    /// Run diagnostic checks on all plugin directories.
    ///
    /// Unlike `list_plugins()`, this scans the plugins directory directly so it
    /// can report on plugins that failed to load (e.g. invalid manifests).
    pub fn doctor(&self) -> Vec<PluginDiagnostic> {
        let mut diagnostics = Vec::new();

        if !self.plugins_dir.exists() {
            return diagnostics;
        }

        let entries = match std::fs::read_dir(&self.plugins_dir) {
            Ok(e) => e,
            Err(_) => return diagnostics,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            diagnostics.push(self.diagnose_plugin(&path));
        }

        diagnostics.sort_by(|a, b| a.plugin_name.cmp(&b.plugin_name));
        diagnostics
    }

    /// Returns the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginCapability, PluginPermission};
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
        let alpha = host
            .get_plugin("alpha")
            .expect("alpha should be discovered");
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

        let gamma = host
            .get_plugin("gamma")
            .expect("gamma should be discovered");
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
        let info2 = host2
            .get_plugin("hash-reload")
            .expect("plugin should exist after reload");
        let new_hash = info2
            .wasm_sha256
            .clone()
            .expect("hash should be recorded after reload");

        assert_ne!(
            original_hash, new_hash,
            "hash must change when WASM binary changes"
        );

        // Verify the new hash matches the actual file content
        let expected = PluginHost::compute_wasm_hash(&plugin_dir.join("plugin.wasm")).unwrap();
        assert_eq!(
            new_hash, expected,
            "hash should match freshly computed value"
        );
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

        assert_ne!(
            hash_v1, hash_v2,
            "hash must differ after reinstall with new binary"
        );

        // Verify v2 hash is correct
        let installed_wasm = plugins_base.join("reinstallable").join("plugin.wasm");
        let expected = PluginHost::compute_wasm_hash(&installed_wasm).unwrap();
        assert_eq!(
            hash_v2, expected,
            "reinstall hash should match computed value"
        );
    }

    #[test]
    fn test_install_writes_sha256_sidecar_file() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins_base).unwrap();

        let source_dir = dir.path().join("source");
        std::fs::create_dir_all(&source_dir).unwrap();

        let wasm_content = b"sidecar-test-wasm";
        std::fs::write(
            source_dir.join("manifest.toml"),
            r#"
name = "sidecar-test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();
        std::fs::write(source_dir.join("plugin.wasm"), wasm_content).unwrap();

        let mut host = PluginHost::new(dir.path()).unwrap();
        host.install(source_dir.to_str().unwrap()).unwrap();

        // The .sha256 sidecar file must exist alongside the installed WASM binary.
        let hash_file = plugins_base.join("sidecar-test").join("plugin.wasm.sha256");
        assert!(
            hash_file.exists(),
            ".wasm.sha256 sidecar file must be created at install time"
        );

        // Its contents must match the in-memory hash.
        let file_hash = std::fs::read_to_string(&hash_file).unwrap();
        let mem_hash = host
            .get_plugin("sidecar-test")
            .unwrap()
            .wasm_sha256
            .expect("hash should be set");
        assert_eq!(
            file_hash, mem_hash,
            "sidecar file must contain the same hash as metadata"
        );
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
        let info = host
            .get_plugin("tampered")
            .expect("plugin should be discovered");
        assert!(
            info.wasm_sha256.is_some(),
            "hash must be recorded at discovery time"
        );
        host.verify_wasm_integrity("tampered")
            .expect("integrity check should pass on unmodified binary");

        // Tamper with the WASM binary on disk.
        std::fs::write(plugin_dir.join("plugin.wasm"), b"malicious-replacement").unwrap();

        // verify_wasm_integrity must fail with a clear error.
        let err = host
            .verify_wasm_integrity("tampered")
            .expect_err("integrity check must fail after tampering");

        let msg = err.to_string();
        assert!(
            msg.contains("integrity check failed"),
            "error should mention integrity failure: {msg}"
        );
        assert!(
            msg.contains("tampered"),
            "error should name the plugin: {msg}"
        );
        assert!(
            msg.contains("expected hash"),
            "error should include expected hash: {msg}"
        );
        assert!(
            msg.contains("got"),
            "error should include actual hash: {msg}"
        );
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
        let info = host
            .get_plugin("no-wasm")
            .expect("plugin should be discovered");
        assert!(
            info.wasm_sha256.is_none(),
            "no hash when WASM file is absent"
        );

        // No hash means no mismatch — verify passes.
        host.verify_wasm_integrity("no-wasm")
            .expect("no hash means no mismatch");
    }

    #[test]
    fn test_reload_rescans_and_reinstantiates() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // Start with one plugin
        let plugin_a = plugins_base.join("alpha");
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::write(
            plugin_a.join("manifest.toml"),
            r#"
name = "alpha"
version = "1.0.0"
wasm_path = "alpha.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        let mut host = PluginHost::new(dir.path()).unwrap();
        assert_eq!(host.list_plugins().len(), 1);

        // Add a second plugin on disk after initial discovery
        let plugin_b = plugins_base.join("beta");
        std::fs::create_dir_all(&plugin_b).unwrap();
        std::fs::write(
            plugin_b.join("manifest.toml"),
            r#"
name = "beta"
version = "0.1.0"
wasm_path = "beta.wasm"
capabilities = ["channel"]
"#,
        )
        .unwrap();

        // Before reload, only alpha is known
        assert_eq!(host.list_plugins().len(), 1);
        assert!(host.get_plugin("beta").is_none());

        // Reload should re-scan and discover beta
        let summary = host.reload().unwrap();
        assert_eq!(host.list_plugins().len(), 2);
        assert!(host.get_plugin("alpha").is_some());
        assert!(host.get_plugin("beta").is_some());
        assert_eq!(summary.total, 2);
        assert!(summary.loaded.contains(&"beta".to_string()));
        assert!(summary.unloaded.is_empty());
        assert!(summary.failed.is_empty());
    }

    #[test]
    fn test_reload_drops_removed_plugins() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        let plugin_dir = plugins_base.join("ephemeral");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "ephemeral"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        let mut host = PluginHost::new(dir.path()).unwrap();
        assert_eq!(host.list_plugins().len(), 1);

        // Remove the plugin directory from disk
        std::fs::remove_dir_all(&plugin_dir).unwrap();

        let summary = host.reload().unwrap();
        assert!(host.list_plugins().is_empty());
        assert_eq!(summary.total, 0);
        assert!(summary.unloaded.contains(&"ephemeral".to_string()));
        assert!(summary.loaded.is_empty());
    }

    #[test]
    fn test_reload_full_cycle() {
        // Full reload-cycle test: add → remove → modify manifest
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // Start with plugin alpha
        let plugin_a = plugins_base.join("alpha");
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::write(
            plugin_a.join("manifest.toml"),
            r#"
name = "alpha"
version = "1.0.0"
description = "Original alpha"
wasm_path = "alpha.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();

        let mut host = PluginHost::new(dir.path()).unwrap();
        assert_eq!(host.list_plugins().len(), 1);
        assert!(host.get_plugin("alpha").is_some());
        assert!(host.get_plugin("beta").is_none());

        // --- Phase 1: add beta, reload ---
        let plugin_b = plugins_base.join("beta");
        std::fs::create_dir_all(&plugin_b).unwrap();
        std::fs::write(
            plugin_b.join("manifest.toml"),
            r#"
name = "beta"
version = "0.1.0"
wasm_path = "beta.wasm"
capabilities = ["channel"]
"#,
        )
        .unwrap();

        let summary = host.reload().unwrap();
        assert!(host.get_plugin("alpha").is_some());
        assert!(host.get_plugin("beta").is_some());
        assert_eq!(host.list_plugins().len(), 2);
        assert!(summary.loaded.contains(&"beta".to_string()));
        assert!(summary.unloaded.is_empty());

        // --- Phase 2: remove alpha, reload ---
        std::fs::remove_dir_all(&plugin_a).unwrap();

        let summary = host.reload().unwrap();
        assert!(host.get_plugin("alpha").is_none());
        assert!(host.get_plugin("beta").is_some());
        assert_eq!(host.list_plugins().len(), 1);
        assert!(summary.unloaded.contains(&"alpha".to_string()));
        assert!(summary.loaded.is_empty());

        // --- Phase 3: re-add alpha with modified manifest, reload ---
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::write(
            plugin_a.join("manifest.toml"),
            r#"
name = "alpha"
version = "2.0.0"
description = "Modified alpha"
wasm_path = "alpha.wasm"
capabilities = ["tool", "channel"]
"#,
        )
        .unwrap();

        let summary = host.reload().unwrap();
        assert_eq!(host.list_plugins().len(), 2);
        assert!(summary.loaded.contains(&"alpha".to_string()));
        let alpha = host.get_plugin("alpha").unwrap();
        assert_eq!(alpha.version, "2.0.0");
        assert_eq!(alpha.description.as_deref(), Some("Modified alpha"));
        assert_eq!(
            alpha.capabilities,
            vec![PluginCapability::Tool, PluginCapability::Channel]
        );
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

    #[test]
    fn test_doctor_checks_all_installed_plugins() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // Plugin A — valid, with WASM file
        let plugin_a = plugins_base.join("alpha");
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::write(
            plugin_a.join("manifest.toml"),
            r#"
name = "alpha"
version = "1.0.0"
wasm_path = "alpha.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();
        std::fs::write(plugin_a.join("alpha.wasm"), b"wasm-alpha").unwrap();

        // Plugin B — valid manifest but missing WASM file
        let plugin_b = plugins_base.join("beta");
        std::fs::create_dir_all(&plugin_b).unwrap();
        std::fs::write(
            plugin_b.join("manifest.toml"),
            r#"
name = "beta"
version = "0.1.0"
wasm_path = "beta.wasm"
capabilities = ["channel"]
"#,
        )
        .unwrap();

        // Plugin C — invalid manifest
        let plugin_c = plugins_base.join("gamma");
        std::fs::create_dir_all(&plugin_c).unwrap();
        std::fs::write(plugin_c.join("manifest.toml"), "not valid toml {{{").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let diagnostics = host.doctor();

        // doctor() must return a diagnostic for every plugin directory
        assert_eq!(
            diagnostics.len(),
            3,
            "doctor() must check all 3 installed plugins"
        );

        // Results are sorted by name
        assert_eq!(diagnostics[0].plugin_name, "alpha");
        assert_eq!(diagnostics[1].plugin_name, "beta");
        assert_eq!(diagnostics[2].plugin_name, "gamma");

        // Alpha should be at worst Warn (valid manifest + WASM exists, but no hash sidecar)
        assert_ne!(
            diagnostics[0].overall(),
            DiagStatus::Fail,
            "alpha should not fail"
        );

        // Beta should fail (missing WASM file)
        assert!(
            diagnostics[1]
                .checks
                .iter()
                .any(|c| c.name == "wasm_file" && c.status == DiagStatus::Fail),
            "beta should fail the wasm_file check"
        );

        // Gamma should fail (invalid manifest)
        assert_eq!(diagnostics[2].overall(), DiagStatus::Fail);
        assert!(
            diagnostics[2]
                .checks
                .iter()
                .any(|c| c.name == "manifest" && c.status == DiagStatus::Fail),
            "gamma should fail the manifest check"
        );
    }

    #[test]
    fn test_diagnose_plugin_reports_missing_config_keys_with_names() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // Plugin with required config keys (no values provided at runtime)
        let plugin_dir = plugins_base.join("needs-config");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "needs-config"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[config.api_key]
required = true

[config.api_secret]
required = true

[config.optional_flag]
required = false
"#,
        )
        .unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"fake-wasm").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let diag = host.diagnose_plugin(&plugin_dir);

        // The diagnostic must carry the plugin name
        assert_eq!(diag.plugin_name, "needs-config");

        // Find the config check
        let config_check = diag
            .checks
            .iter()
            .find(|c| c.name == "config")
            .expect("diagnose_plugin must produce a config check");

        // It should be a warning (required keys declared but not verified at runtime)
        assert_eq!(
            config_check.status,
            DiagStatus::Warn,
            "missing required config keys should warn"
        );

        // The message must include both key names
        assert!(
            config_check.message.contains("api_key"),
            "config diagnostic must name the missing key 'api_key': {}",
            config_check.message
        );
        assert!(
            config_check.message.contains("api_secret"),
            "config diagnostic must name the missing key 'api_secret': {}",
            config_check.message
        );

        // optional_flag should NOT appear (it's not required)
        assert!(
            !config_check.message.contains("optional_flag"),
            "non-required config keys must not be reported: {}",
            config_check.message
        );
    }

    #[test]
    fn test_diagnose_plugin_reports_capability_conflicts() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");

        // --- Plugin A: wildcard hosts only → Warn ---
        let plugin_a = plugins_base.join("wildcard-host");
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::write(
            plugin_a.join("manifest.toml"),
            r#"
name = "wildcard-host"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
allowed_hosts = ["*.example.com"]
"#,
        )
        .unwrap();
        std::fs::write(plugin_a.join("plugin.wasm"), b"fake-wasm").unwrap();

        // --- Plugin B: forbidden path only → Fail ---
        let plugin_b = plugins_base.join("forbidden-path");
        std::fs::create_dir_all(&plugin_b).unwrap();
        std::fs::write(
            plugin_b.join("manifest.toml"),
            r#"
name = "forbidden-path"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[allowed_paths]
secrets = "/etc/shadow"
"#,
        )
        .unwrap();
        std::fs::write(plugin_b.join("plugin.wasm"), b"fake-wasm").unwrap();

        // --- Plugin C: wildcard hosts AND forbidden paths → Fail ---
        let plugin_c = plugins_base.join("both-bad");
        std::fs::create_dir_all(&plugin_c).unwrap();
        std::fs::write(
            plugin_c.join("manifest.toml"),
            r#"
name = "both-bad"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
allowed_hosts = ["*"]

[allowed_paths]
sys = "/etc/config"
"#,
        )
        .unwrap();
        std::fs::write(plugin_c.join("plugin.wasm"), b"fake-wasm").unwrap();

        // --- Plugin D: no conflicts → Pass ---
        let plugin_d = plugins_base.join("clean-plugin");
        std::fs::create_dir_all(&plugin_d).unwrap();
        std::fs::write(
            plugin_d.join("manifest.toml"),
            r#"
name = "clean-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
allowed_hosts = ["api.example.com"]

[allowed_paths]
workspace = "/tmp/plugin-data"
"#,
        )
        .unwrap();
        std::fs::write(plugin_d.join("plugin.wasm"), b"fake-wasm").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();

        // Plugin A: wildcard hosts → Warn
        let diag_a = host.diagnose_plugin(&plugin_a);
        let cap_a = diag_a
            .checks
            .iter()
            .find(|c| c.name == "capabilities")
            .expect("must produce a capabilities check");
        assert_eq!(
            cap_a.status,
            DiagStatus::Warn,
            "wildcard hosts should warn: {}",
            cap_a.message
        );
        assert!(
            cap_a.message.contains("wildcard"),
            "message must mention wildcard: {}",
            cap_a.message
        );

        // Plugin B: forbidden path → Fail
        let diag_b = host.diagnose_plugin(&plugin_b);
        let cap_b = diag_b
            .checks
            .iter()
            .find(|c| c.name == "capabilities")
            .expect("must produce a capabilities check");
        assert_eq!(
            cap_b.status,
            DiagStatus::Fail,
            "forbidden paths should fail: {}",
            cap_b.message
        );
        assert!(
            cap_b.message.contains("forbidden"),
            "message must mention forbidden: {}",
            cap_b.message
        );

        // Plugin C: both wildcard + forbidden → Fail
        let diag_c = host.diagnose_plugin(&plugin_c);
        let cap_c = diag_c
            .checks
            .iter()
            .find(|c| c.name == "capabilities")
            .expect("must produce a capabilities check");
        assert_eq!(
            cap_c.status,
            DiagStatus::Fail,
            "both bad should fail: {}",
            cap_c.message
        );
        assert!(
            cap_c.message.contains("wildcard") && cap_c.message.contains("forbidden"),
            "message must mention both wildcard and forbidden: {}",
            cap_c.message
        );

        // Plugin D: clean → Pass
        let diag_d = host.diagnose_plugin(&plugin_d);
        let cap_d = diag_d
            .checks
            .iter()
            .find(|c| c.name == "capabilities")
            .expect("must produce a capabilities check");
        assert_eq!(
            cap_d.status,
            DiagStatus::Pass,
            "clean plugin should pass: {}",
            cap_d.message
        );
        assert!(
            cap_d.message.contains("no capability conflicts"),
            "message: {}",
            cap_d.message
        );
    }
}
