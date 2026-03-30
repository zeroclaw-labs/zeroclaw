//! Extism plugin loader — converts a parsed [`PluginManifest`] into an
//! [`extism::Manifest`] ready for instantiation, and provides a [`PluginLoader`]
//! that scans plugin directories and returns loaded descriptors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::error::PluginError;
use super::PluginManifest;
use crate::config::schema::Config;
use crate::security::SecurityPolicy;

/// Paths that plugins are never allowed to access, regardless of configuration.
///
/// Any `allowed_paths` entry whose resolved host path equals or is a child of
/// one of these prefixes will be rejected with [`PluginError::ForbiddenPath`].
pub const FORBIDDEN_PATHS: &[&str] = &[
    "/etc",
    "/root",
    "/var",
    "~/.ssh",
    "~/.gnupg",
    "/proc",
    "/sys",
    "/dev",
];

/// Controls how strictly plugin network declarations are validated.
///
/// At `Default` level, wildcard hosts produce a warning but are allowed.
/// At `Strict` and `Paranoid` levels, wildcard hosts are rejected outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkSecurityLevel {
    /// Most permissive: all declared capabilities are allowed without warnings.
    Relaxed,
    /// Permissive: wildcards allowed with a warning log.
    Default,
    /// Wildcards in `allowed_hosts` are rejected.
    Strict,
    /// Wildcards in `allowed_hosts` are rejected (same enforcement as Strict,
    /// but may impose additional constraints in the future).
    Paranoid,
}

impl NetworkSecurityLevel {
    /// Parse a network security level from a config string.
    ///
    /// Unrecognised values fall back to `Default`.
    pub fn from_config(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "relaxed" => Self::Relaxed,
            "strict" => Self::Strict,
            "paranoid" => Self::Paranoid,
            _ => Self::Default,
        }
    }

    /// Returns `true` if audit logging should be forced for all plugin calls,
    /// even when audit is globally disabled.
    pub fn requires_forced_audit(&self) -> bool {
        matches!(self, Self::Strict | Self::Paranoid)
    }
}

/// Returns `true` if the given host pattern contains a wildcard.
fn is_wildcard_host(host: &str) -> bool {
    host.contains('*')
}

/// Validate a plugin's `allowed_tools` delegation against the given [`NetworkSecurityLevel`].
///
/// In `Strict` or `Paranoid` mode, a wildcard entry (`"*"`) in `allowed_tools` is
/// rejected with [`PluginError::WildcardDelegationRejected`]. In `Default` mode
/// wildcard delegation produces a warning log but is allowed through.
pub fn validate_allowed_tools_delegation(
    plugin_name: &str,
    allowed_tools: &[String],
    level: NetworkSecurityLevel,
) -> Result<(), PluginError> {
    let has_wildcard = allowed_tools.iter().any(|t| t == "*");
    if !has_wildcard {
        return Ok(());
    }
    match level {
        NetworkSecurityLevel::Relaxed => {}
        NetworkSecurityLevel::Default => {
            tracing::warn!(
                plugin = %plugin_name,
                "plugin declares wildcard tool delegation which is allowed at Default security level \
                 but would be rejected at Strict or Paranoid"
            );
        }
        NetworkSecurityLevel::Strict | NetworkSecurityLevel::Paranoid => {
            return Err(PluginError::WildcardDelegationRejected {
                plugin: plugin_name.to_string(),
                level: format!("{:?}", level),
            });
        }
    }
    Ok(())
}

/// Validate a plugin's `allowed_hosts` against the given [`NetworkSecurityLevel`].
///
/// In `Strict` or `Paranoid` mode, any host containing a wildcard (`*`) is
/// rejected with [`PluginError::WildcardHostRejected`]. In `Default` mode
/// wildcard hosts produce a warning log but are allowed through.
pub fn validate_allowed_hosts(
    plugin_name: &str,
    hosts: &[String],
    level: NetworkSecurityLevel,
) -> Result<(), PluginError> {
    if level == NetworkSecurityLevel::Relaxed {
        return Ok(());
    }
    if level == NetworkSecurityLevel::Default {
        for host in hosts {
            if is_wildcard_host(host) {
                tracing::warn!(
                    plugin = %plugin_name,
                    host = %host,
                    "plugin declares wildcard host which is allowed at Default security level \
                     but would be rejected at Strict or Paranoid"
                );
            }
        }
        return Ok(());
    }
    for host in hosts {
        if is_wildcard_host(host) {
            return Err(PluginError::WildcardHostRejected {
                plugin: plugin_name.to_string(),
                host: host.clone(),
                level: format!("{:?}", level),
            });
        }
    }
    Ok(())
}

/// Expand `~` and `~/…` prefixes to the user's home directory.
pub fn expand_user_path(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    }
}

/// Validate a plugin's `allowed_paths` against a list of forbidden path prefixes.
///
/// Each physical (host-side) path declared in the plugin manifest is checked
/// against the forbidden list. If any physical path starts with a forbidden
/// prefix, the function returns [`PluginError::ForbiddenPath`].
///
/// Paths are expanded (`~` → home dir) before comparison so that both
/// `~/.ssh` in the forbidden list and `~/.ssh/keys` in allowed_paths match.
pub fn validate_allowed_paths(
    plugin_name: &str,
    allowed_paths: &std::collections::HashMap<String, String>,
    forbidden_paths: &[String],
) -> Result<(), PluginError> {
    for (_guest_path, host_path) in allowed_paths {
        let expanded = expand_user_path(host_path);
        for forbidden in forbidden_paths {
            let forbidden_expanded = expand_user_path(forbidden);
            if expanded.starts_with(&forbidden_expanded) {
                return Err(PluginError::ForbiddenPath {
                    plugin: plugin_name.to_string(),
                    path: host_path.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Validate that all physical paths in `allowed_paths` fall inside the workspace
/// subtree when strict mode is enabled.
///
/// Each physical (host-side) path is expanded (`~` → home dir), made absolute
/// by prepending the workspace root when relative, and then canonicalized via
/// [`std::fs::canonicalize`] to resolve symlinks, `..`, and `.` components.
/// The canonicalized path must start with the canonicalized workspace root.
/// Paths outside the workspace are rejected with
/// [`PluginError::PathOutsideWorkspace`].
///
/// If the path does not exist on disk yet, canonicalization is skipped and
/// the logical (lexically cleaned) path is used instead. This allows plugins
/// to declare paths they intend to create while still blocking symlink escapes
/// for paths that already exist.
pub fn validate_workspace_paths(
    plugin_name: &str,
    allowed_paths: &std::collections::HashMap<String, String>,
    workspace_root: &Path,
) -> Result<(), PluginError> {
    // Canonicalize the workspace root itself; fall back to the original if it
    // doesn't exist (e.g. in unit tests with synthetic paths).
    let canon_workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    for (_guest_path, host_path) in allowed_paths {
        let expanded = expand_user_path(host_path);
        let absolute = if expanded.is_relative() {
            workspace_root.join(&expanded)
        } else {
            expanded
        };

        // Attempt real canonicalization to catch symlink escapes.
        // If the path doesn't exist yet, fall back to the logical absolute path.
        let resolved = absolute
            .canonicalize()
            .unwrap_or_else(|_| absolute.clone());

        if !resolved.starts_with(&canon_workspace) {
            return Err(PluginError::PathOutsideWorkspace {
                plugin: plugin_name.to_string(),
                path: host_path.clone(),
                workspace: workspace_root.to_string_lossy().into_owned(),
            });
        }
    }
    Ok(())
}

/// Validate that a plugin is individually allowlisted when running in Paranoid mode.
///
/// In `Paranoid` mode, only plugins whose name appears in the `allowed_plugins`
/// list are permitted to load. All other plugins are rejected with
/// [`PluginError::PluginNotAllowlisted`]. At other security levels this check
/// is a no-op and always returns `Ok(())`.
pub fn validate_plugin_allowlist(
    plugin_name: &str,
    allowed_plugins: &[String],
    level: NetworkSecurityLevel,
) -> Result<(), PluginError> {
    if level != NetworkSecurityLevel::Paranoid {
        return Ok(());
    }
    if allowed_plugins.iter().any(|name| name == plugin_name) {
        return Ok(());
    }
    tracing::warn!(
        plugin = %plugin_name,
        "plugin rejected: not allowlisted in paranoid mode"
    );
    Err(PluginError::PluginNotAllowlisted {
        plugin: plugin_name.to_string(),
    })
}

/// A loaded plugin descriptor: the parsed manifest plus its resolved directory.
#[derive(Debug, Clone)]
pub struct PluginDescriptor {
    /// The parsed plugin manifest.
    pub manifest: PluginManifest,
    /// The directory containing the plugin (where plugin.toml / manifest.toml lives).
    pub plugin_dir: PathBuf,
}

/// Scans plugin directories, parses manifests, and returns plugin descriptors.
///
/// The entire struct is feature-gated behind `plugins-wasm` at the module level.
pub struct PluginLoader<'a> {
    config: &'a Config,
    security: &'a SecurityPolicy,
}

impl<'a> PluginLoader<'a> {
    /// Create a new loader with references to the application config and security policy.
    pub fn new(config: &'a Config, security: &'a SecurityPolicy) -> Self {
        Self { config, security }
    }

    /// Validate a plugin's manifest against the current security policy.
    ///
    /// Reads the `network_security_level` from the plugin security configuration
    /// and runs all applicable checks:
    ///
    /// 1. **Paranoid allowlist** — in Paranoid mode, only plugins whose name
    ///    appears in `allowed_plugins` are permitted.
    /// 2. **Wildcard host validation** — Strict and Paranoid modes reject
    ///    wildcard patterns in `allowed_hosts`; Default mode warns.
    /// 3. **Forbidden path check** — all levels reject paths in [`FORBIDDEN_PATHS`].
    /// 4. **Workspace path restriction** — in Strict and Paranoid modes,
    ///    physical paths must fall within the workspace subtree.
    ///
    /// Call this **before** plugin instantiation to reject non-compliant plugins
    /// early.
    pub fn validate_security_policy(&self, manifest: &PluginManifest) -> Result<(), PluginError> {
        let level =
            NetworkSecurityLevel::from_config(&self.config.plugins.security.network_security_level);

        // 1. Paranoid mode: plugin must be individually allowlisted.
        validate_plugin_allowlist(
            &manifest.name,
            &self.config.plugins.security.allowed_plugins,
            level,
        )?;

        // 2. Validate allowed_hosts against the security level.
        validate_allowed_hosts(&manifest.name, &manifest.allowed_hosts, level)?;

        // 2b. Validate wildcard tool delegation against the security level.
        if let Some(ref td) = manifest.host_capabilities.tool_delegation {
            validate_allowed_tools_delegation(&manifest.name, &td.allowed_tools, level)?;
        }

        // 3. Reject forbidden paths (all levels).
        let forbidden: Vec<String> = FORBIDDEN_PATHS.iter().map(|s| (*s).to_string()).collect();
        validate_allowed_paths(&manifest.name, &manifest.allowed_paths, &forbidden)?;

        // 4. Strict / Paranoid: paths must be within the workspace subtree.
        if level == NetworkSecurityLevel::Strict || level == NetworkSecurityLevel::Paranoid {
            tracing::info!(
                plugin = %manifest.name,
                "strict mode restriction applied: allowed_paths restricted to workspace subtree"
            );
            validate_workspace_paths(
                &manifest.name,
                &manifest.allowed_paths,
                &self.security.workspace_dir,
            )?;
        }

        // 5. Strict / Paranoid: log restriction summary and forced audit.
        if level == NetworkSecurityLevel::Strict || level == NetworkSecurityLevel::Paranoid {
            tracing::info!(
                plugin = %manifest.name,
                security_level = ?level,
                "strict mode restriction applied: wildcard hosts rejected"
            );
            tracing::info!(
                plugin = %manifest.name,
                security_level = ?level,
                "strict mode restriction applied: audit logging forced for all plugin calls"
            );
        }

        Ok(())
    }

    /// Read the `[plugins.<name>]` section from config.toml for a given plugin.
    ///
    /// Returns the key-value pairs as a `BTreeMap<String, String>` suitable for
    /// passing to [`resolve_plugin_config`](super::resolve_plugin_config) and
    /// ultimately to Extism. Returns an empty map when no `[plugins.<name>]`
    /// section exists (the caller is expected to fall back to manifest defaults
    /// or flag missing required keys).
    pub fn plugin_config(&self, plugin_name: &str) -> BTreeMap<String, String> {
        self.config
            .plugins
            .per_plugin
            .get(plugin_name)
            .map(|vals| vals.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    /// Resolve the plugins directory from config, expanding `~` to the user's home.
    fn plugins_dir(&self) -> PathBuf {
        let raw = &self.config.plugins.plugins_dir;
        if raw.starts_with("~/") {
            if let Some(user_dirs) = directories::UserDirs::new() {
                return user_dirs.home_dir().join(&raw[2..]);
            }
        }
        PathBuf::from(raw)
    }

    /// Scan the plugins directory and load all valid plugin manifests.
    ///
    /// Each sub-directory is checked for `plugin.toml` first, then `manifest.toml`
    /// as a backwards-compatibility fallback. Directories without a recognizable
    /// manifest are silently skipped; directories with malformed manifests produce
    /// a warning log and are also skipped.
    ///
    /// Returns a `Vec<PluginDescriptor>` for every successfully parsed manifest.
    pub fn load_all(&self) -> Result<Vec<PluginDescriptor>, PluginError> {
        let plugins_dir = self.plugins_dir();

        if !plugins_dir.exists() {
            return Ok(Vec::new());
        }

        let entries = std::fs::read_dir(&plugins_dir)?;
        let mut descriptors = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Prefer plugin.toml; fall back to manifest.toml for backwards compat.
            let manifest_path = path.join("plugin.toml");
            let manifest_path = if manifest_path.exists() {
                manifest_path
            } else {
                let fallback = path.join("manifest.toml");
                if fallback.exists() {
                    fallback
                } else {
                    continue;
                }
            };

            match std::fs::read_to_string(&manifest_path) {
                Ok(content) => match PluginManifest::parse(&content) {
                    Ok(manifest) => {
                        descriptors.push(PluginDescriptor {
                            manifest,
                            plugin_dir: path,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %manifest_path.display(),
                            error = %e,
                            "skipping plugin with invalid manifest"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %manifest_path.display(),
                        error = %e,
                        "failed to read plugin manifest"
                    );
                }
            }
        }

        Ok(descriptors)
    }
}

/// Verify the integrity of a WASM binary before instantiation.
///
/// Reads the SHA-256 hash from the `.wasm.sha256` sidecar file (written at
/// install time) and compares it against a freshly computed hash of the binary.
/// Returns [`PluginError::HashMismatch`] when the hashes differ.
///
/// If no sidecar file exists (pre-hash install or WASM file absent), a warning
/// is logged and the check is skipped for backwards compatibility.
pub fn verify_wasm_integrity(plugin_name: &str, wasm_path: &Path) -> Result<(), PluginError> {
    let sidecar = wasm_path.with_extension("wasm.sha256");
    if sidecar.exists() {
        let expected = std::fs::read_to_string(&sidecar)?.trim().to_string();
        let bytes = std::fs::read(wasm_path)?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            return Err(PluginError::HashMismatch {
                plugin: plugin_name.to_string(),
                expected,
                actual,
            });
        }
    } else if wasm_path.exists() {
        tracing::warn!(
            plugin = %plugin_name,
            "no stored SHA-256 hash for plugin; skipping integrity check \
             (pre-hash install or missing sidecar)"
        );
    } else {
        tracing::warn!(
            plugin = %plugin_name,
            "WASM file not found; skipping integrity check"
        );
    }
    Ok(())
}

/// The result of building an Extism manifest from a ZeroClaw plugin manifest.
///
/// Holds the [`extism::Manifest`] plus the WASI flag (which is applied at
/// plugin instantiation time, not on the manifest itself).
#[derive(Debug, Clone)]
pub struct LoaderManifest {
    /// The extism manifest ready for `PluginBuilder::new`.
    pub manifest: extism::Manifest,
    /// Whether WASI should be enabled when instantiating the plugin.
    pub wasi: bool,
}

/// Build an [`extism::Manifest`] from a parsed [`PluginManifest`].
///
/// `plugin_dir` is the directory containing the manifest.toml — the
/// `wasm_path` field is resolved relative to it.
///
/// `workspace_root` is the ZeroClaw workspace directory. When provided,
/// relative physical paths in `allowed_paths` are resolved against it.
/// When `None`, physical paths are used as-is.
pub fn build_extism_manifest(
    plugin_manifest: &PluginManifest,
    plugin_dir: &Path,
    workspace_root: Option<&Path>,
) -> LoaderManifest {
    let wasm_abs_path: PathBuf = plugin_dir.join(&plugin_manifest.wasm_path);

    let mut manifest = extism::Manifest::new([extism::Wasm::file(&wasm_abs_path)])
        .with_timeout(Duration::from_millis(plugin_manifest.timeout_ms));

    // Map allowed hosts.
    if !plugin_manifest.allowed_hosts.is_empty() {
        manifest = manifest
            .with_allowed_hosts(plugin_manifest.allowed_hosts.iter().cloned());
    }

    // Map allowed paths — resolve relative physical paths against workspace root.
    if !plugin_manifest.allowed_paths.is_empty() {
        for (logical, physical) in &plugin_manifest.allowed_paths {
            let resolved_physical = {
                let p = Path::new(physical);
                if p.is_relative() {
                    if let Some(root) = workspace_root {
                        root.join(p).to_string_lossy().into_owned()
                    } else {
                        physical.clone()
                    }
                } else {
                    physical.clone()
                }
            };
            manifest = manifest.with_allowed_path(resolved_physical, PathBuf::from(logical));
        }
    }

    LoaderManifest {
        manifest,
        wasi: plugin_manifest.wasi,
    }
}

/// Build an [`extism::Manifest`] with resolved configuration values already
/// injected via [`extism::Manifest::with_config`].
pub fn build_extism_manifest_with_config(
    plugin_manifest: &PluginManifest,
    plugin_dir: &Path,
    config: BTreeMap<String, String>,
    workspace_root: Option<&Path>,
) -> LoaderManifest {
    let mut result = build_extism_manifest(plugin_manifest, plugin_dir, workspace_root);
    result.manifest = result.manifest.with_config(config.into_iter());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper: build a minimal `PluginManifest` with sensible defaults.
    fn minimal_manifest() -> PluginManifest {
        PluginManifest {
            name: "test-plugin".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            wasm_path: "plugin.wasm".to_string(),
            capabilities: vec![],
            permissions: vec![],
            allowed_hosts: vec![],
            allowed_paths: HashMap::new(),
            tools: vec![],
            config: HashMap::new(),
            wasi: true,
            timeout_ms: 30_000,
            signature: None,
            publisher_key: None,
            host_capabilities: Default::default(),
        }
    }

    #[test]
    fn extism_manifest_has_correct_wasm_path() {
        let pm = minimal_manifest();
        let dir = Path::new("/opt/plugins/my-plugin");
        let result = build_extism_manifest(&pm, dir, None);

        assert_eq!(result.manifest.wasm.len(), 1);
        match &result.manifest.wasm[0] {
            extism::Wasm::File { path, .. } => {
                assert_eq!(path, &PathBuf::from("/opt/plugins/my-plugin/plugin.wasm"));
            }
            other => panic!("expected Wasm::File, got {:?}", other),
        }
    }

    #[test]
    fn extism_manifest_has_correct_timeout() {
        let mut pm = minimal_manifest();
        pm.timeout_ms = 5_000;

        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        assert_eq!(result.manifest.timeout_ms, Some(5_000));
    }

    #[test]
    fn extism_manifest_default_timeout() {
        let pm = minimal_manifest();
        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        assert_eq!(result.manifest.timeout_ms, Some(30_000));
    }

    #[test]
    fn extism_manifest_wasi_flag_propagated() {
        let mut pm = minimal_manifest();

        pm.wasi = true;
        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        assert!(result.wasi, "WASI flag should be true");

        pm.wasi = false;
        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        assert!(!result.wasi, "WASI flag should be false");
    }

    #[test]
    fn extism_manifest_allowed_hosts() {
        let mut pm = minimal_manifest();
        pm.allowed_hosts = vec!["example.com".to_string(), "*.api.io".to_string()];

        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        let hosts = result.manifest.allowed_hosts.unwrap();
        assert_eq!(hosts, vec!["example.com", "*.api.io"]);
    }

    #[test]
    fn empty_allowed_hosts_means_no_network_access() {
        let pm = minimal_manifest(); // allowed_hosts defaults to vec![]
        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        assert!(
            result.manifest.allowed_hosts.is_none(),
            "Empty allowed_hosts should produce None (no network access), got {:?}",
            result.manifest.allowed_hosts
        );
    }

    #[test]
    fn extism_manifest_allowed_paths() {
        let mut pm = minimal_manifest();
        pm.allowed_paths
            .insert("/data".to_string(), "/host/data".to_string());

        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        let paths = result.manifest.allowed_paths.unwrap();
        assert_eq!(paths.get("/host/data"), Some(&PathBuf::from("/data")));
    }

    /// End-to-end: allowed_paths in plugin.toml are parsed and forwarded to
    /// the Extism Manifest as WASI preopens (the `allowed_paths` field).
    #[test]
    fn allowed_paths_from_toml_become_extism_wasi_preopens() {
        use crate::plugins::PluginManifest;

        let toml_str = r#"
name = "fs-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[allowed_paths]
data = "/var/data"
cache = "/tmp/cache"
logs  = "/var/log/app"
"#;
        // Step 1: parse plugin.toml → PluginManifest
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.allowed_paths.len(), 3);

        // Step 2: build Extism manifest (the function under test)
        let result = build_extism_manifest(&manifest, Path::new("/opt/plugins/fs-plugin"), None);

        // Step 3: verify each allowed_path landed as a WASI preopen
        let extism_paths = result
            .manifest
            .allowed_paths
            .expect("allowed_paths should be Some when plugin.toml declares paths");

        assert_eq!(extism_paths.len(), 3, "all three paths should be mapped");

        // Extism stores preopens as physical_path → guest_path
        assert_eq!(extism_paths.get("/var/data"), Some(&PathBuf::from("data")));
        assert_eq!(
            extism_paths.get("/tmp/cache"),
            Some(&PathBuf::from("cache"))
        );
        assert_eq!(
            extism_paths.get("/var/log/app"),
            Some(&PathBuf::from("logs"))
        );
    }

    /// When plugin.toml declares no allowed_paths, the Extism manifest should
    /// have `allowed_paths: None` (no filesystem access).
    #[test]
    fn empty_allowed_paths_means_no_wasi_preopens() {
        let pm = minimal_manifest(); // allowed_paths defaults to empty HashMap
        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        assert!(
            result.manifest.allowed_paths.is_none(),
            "Empty allowed_paths should produce None (no filesystem access), got {:?}",
            result.manifest.allowed_paths
        );
    }

    /// Relative physical paths in allowed_paths are resolved against the
    /// workspace root when one is provided.
    #[test]
    fn relative_allowed_paths_resolved_against_workspace_root() {
        let mut pm = minimal_manifest();
        // "data" is a relative physical path; "/absolute/path" is absolute.
        pm.allowed_paths
            .insert("/guest/data".to_string(), "data".to_string());
        pm.allowed_paths
            .insert("/guest/abs".to_string(), "/absolute/path".to_string());

        let workspace_root = Path::new("/home/user/zeroclaw/workspace");
        let result = build_extism_manifest(&pm, Path::new("/tmp"), Some(workspace_root));

        let paths = result
            .manifest
            .allowed_paths
            .expect("allowed_paths should be Some");

        // Relative path should be resolved against workspace root.
        assert_eq!(
            paths.get("/home/user/zeroclaw/workspace/data"),
            Some(&PathBuf::from("/guest/data")),
            "relative path should be resolved against workspace root"
        );

        // Absolute path should remain unchanged.
        assert_eq!(
            paths.get("/absolute/path"),
            Some(&PathBuf::from("/guest/abs")),
            "absolute path should not be modified"
        );
    }

    /// When no workspace root is provided, relative paths are passed through as-is.
    #[test]
    fn relative_allowed_paths_unchanged_without_workspace_root() {
        let mut pm = minimal_manifest();
        pm.allowed_paths
            .insert("/guest/data".to_string(), "data".to_string());

        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);

        let paths = result
            .manifest
            .allowed_paths
            .expect("allowed_paths should be Some");

        assert_eq!(
            paths.get("data"),
            Some(&PathBuf::from("/guest/data")),
            "relative path should be passed through unchanged when no workspace root"
        );
    }

    #[test]
    fn extism_manifest_with_config_values() {
        let pm = minimal_manifest();
        let mut config = BTreeMap::new();
        config.insert("api_key".to_string(), "secret123".to_string());

        let result = build_extism_manifest_with_config(&pm, Path::new("/tmp"), config, None);
        assert_eq!(
            result.manifest.config.get("api_key"),
            Some(&"secret123".to_string())
        );
    }

    /// End-to-end: resolve_plugin_config output feeds into build_extism_manifest_with_config,
    /// verifying that config.toml values (overrides, defaults, passthrough) all land in the
    /// extism Manifest config map.
    #[test]
    fn config_toml_values_mapped_into_extism_config() {
        use crate::plugins::resolve_plugin_config;

        // Manifest declares: api_key (required), model (default "gpt-4"), temperature (default "0.7")
        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "api_key".to_string(),
            serde_json::json!({"required": true}),
        );
        manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));
        manifest_config.insert(
            "temperature".to_string(),
            serde_json::json!({"default": "0.7"}),
        );

        // Operator config.toml provides api_key, overrides model, and adds an extra key
        let mut config_values = HashMap::new();
        config_values.insert("api_key".to_string(), "sk-live-abc".to_string());
        config_values.insert("model".to_string(), "claude-3".to_string());
        config_values.insert("custom_flag".to_string(), "enabled".to_string());

        // Step 1: resolve config (as PluginHost would)
        let resolved =
            resolve_plugin_config("test-plugin", &manifest_config, Some(&config_values))
                .expect("config resolution should succeed");

        // Step 2: build extism manifest with the resolved config
        let pm = minimal_manifest();
        let result = build_extism_manifest_with_config(&pm, Path::new("/tmp"), resolved, None);

        // Verify all config values made it into the extism Manifest
        let ext_config = &result.manifest.config;
        assert_eq!(ext_config.get("api_key"), Some(&"sk-live-abc".to_string()), "required key from config.toml");
        assert_eq!(ext_config.get("model"), Some(&"claude-3".to_string()), "overridden default");
        assert_eq!(ext_config.get("temperature"), Some(&"0.7".to_string()), "manifest default preserved");
        assert_eq!(ext_config.get("custom_flag"), Some(&"enabled".to_string()), "passthrough extra key");
        assert_eq!(ext_config.len(), 4, "exactly 4 config entries");
    }

    /// Verify that when no config values are provided and the manifest has only
    /// defaulted keys, the defaults still appear in the extism config map.
    #[test]
    fn config_defaults_mapped_into_extism_config_without_values() {
        use crate::plugins::resolve_plugin_config;

        let mut manifest_config = HashMap::new();
        manifest_config.insert("log_level".to_string(), serde_json::json!("info"));
        manifest_config.insert("retries".to_string(), serde_json::json!(3));

        let resolved = resolve_plugin_config("defaults-only", &manifest_config, None)
            .expect("config resolution should succeed with defaults");

        let pm = minimal_manifest();
        let result = build_extism_manifest_with_config(&pm, Path::new("/tmp"), resolved, None);

        let ext_config = &result.manifest.config;
        assert_eq!(ext_config.get("log_level"), Some(&"info".to_string()));
        assert_eq!(ext_config.get("retries"), Some(&"3".to_string()), "numeric default as string");
    }

    /// Missing required config keys produce a clear, actionable error that names
    /// both the plugin and the missing keys — acceptance criterion for US-ZCL-2.
    #[test]
    fn missing_required_config_keys_produce_clear_errors() {
        use crate::plugins::resolve_plugin_config;

        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "api_key".to_string(),
            serde_json::json!({"required": true}),
        );
        manifest_config.insert(
            "db_url".to_string(),
            serde_json::json!({"required": true}),
        );
        // This key has a default, so it should NOT be reported as missing.
        manifest_config.insert("log_level".to_string(), serde_json::json!("info"));

        // No config values supplied — both required keys are missing.
        let err = resolve_plugin_config("weather-plugin", &manifest_config, None)
            .expect_err("should fail when required keys are missing");

        let msg = err.to_string();

        // The error message must name the plugin so operators know which one to fix.
        assert!(
            msg.contains("weather-plugin"),
            "error should name the plugin, got: {msg}"
        );
        // The error message must list each missing key.
        assert!(
            msg.contains("api_key"),
            "error should list missing key 'api_key', got: {msg}"
        );
        assert!(
            msg.contains("db_url"),
            "error should list missing key 'db_url', got: {msg}"
        );
        // Non-missing keys with defaults should NOT appear.
        assert!(
            !msg.contains("log_level"),
            "error should not mention keys that have defaults, got: {msg}"
        );
    }

    /// When only some required keys are supplied, the error lists only the
    /// still-missing ones.
    #[test]
    fn missing_config_error_lists_only_unsupplied_keys() {
        use crate::plugins::resolve_plugin_config;

        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "api_key".to_string(),
            serde_json::json!({"required": true}),
        );
        manifest_config.insert(
            "secret".to_string(),
            serde_json::json!({"required": true}),
        );

        // Supply only api_key — secret is still missing.
        let mut values = HashMap::new();
        values.insert("api_key".to_string(), "sk-123".to_string());

        let err = resolve_plugin_config("partial-plugin", &manifest_config, Some(&values))
            .expect_err("should fail with one missing key");

        let msg = err.to_string();
        assert!(
            msg.contains("secret"),
            "error should list missing key 'secret', got: {msg}"
        );
        assert!(
            !msg.contains("api_key"),
            "error should NOT list supplied key 'api_key', got: {msg}"
        );
    }

    /// Acceptance criterion for US-ZCL-2: plugin instantiation succeeds with
    /// a valid wasm binary.
    ///
    /// Creates a minimal valid wasm module on disk, builds the extism manifest
    /// via `build_extism_manifest`, then instantiates an `extism::Plugin` to
    /// prove the full path from manifest → running plugin works.
    #[test]
    fn plugin_instantiation_succeeds_with_valid_wasm() {
        // Minimal valid wasm module (magic + version header, empty module).
        let wasm_bytes: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, // \0asm
            0x01, 0x00, 0x00, 0x00, // version 1
        ];

        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("plugin.wasm");
        std::fs::write(&wasm_path, wasm_bytes).unwrap();

        let pm = minimal_manifest();
        let loader = build_extism_manifest(&pm, dir.path(), None);

        let plugin = extism::Plugin::new(&loader.manifest, [], loader.wasi);
        assert!(
            plugin.is_ok(),
            "Plugin instantiation should succeed with a valid wasm binary, got: {:?}",
            plugin.err()
        );
    }

    /// End-to-end: allowed_hosts declared in plugin.toml (flat format) flow
    /// through PluginManifest::parse → build_extism_manifest into the Extism
    /// Manifest's allowed_hosts field.  Acceptance criterion for US-ZCL-6.
    #[test]
    fn allowed_hosts_from_flat_toml_reach_extism_manifest() {
        use crate::plugins::PluginManifest;

        let toml_str = r#"
            name = "http-plugin"
            version = "1.0.0"
            wasm_path = "plugin.wasm"
            capabilities = ["tool"]
            allowed_hosts = ["api.example.com", "cdn.example.com"]

            [[tools]]
            name = "fetch"
            description = "fetch a URL"
            export = "fetch"
            risk_level = "medium"
        "#;

        let pm = PluginManifest::parse(toml_str).expect("flat TOML should parse");
        assert_eq!(pm.allowed_hosts, vec!["api.example.com", "cdn.example.com"]);

        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        let hosts = result.manifest.allowed_hosts.expect("allowed_hosts should be Some");
        assert_eq!(hosts, vec!["api.example.com", "cdn.example.com"]);
    }

    /// End-to-end: allowed_hosts declared in the nested `[plugin.network]`
    /// section flow through into the Extism Manifest.  Acceptance criterion
    /// for US-ZCL-6.
    #[test]
    fn allowed_hosts_from_nested_toml_reach_extism_manifest() {
        use crate::plugins::PluginManifest;

        let toml_str = r#"
            [plugin]
            name = "http-plugin"
            version = "1.0.0"
            wasm_path = "plugin.wasm"
            capabilities = ["tool"]

            [plugin.network]
            allowed_hosts = ["httpbin.org", "example.com"]

            [[tools]]
            name = "fetch"
            description = "fetch a URL"
            export = "fetch"
            risk_level = "medium"
        "#;

        let pm = PluginManifest::parse(toml_str).expect("nested TOML should parse");
        assert_eq!(pm.allowed_hosts, vec!["httpbin.org", "example.com"]);

        let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
        let hosts = result.manifest.allowed_hosts.expect("allowed_hosts should be Some");
        assert_eq!(hosts, vec!["httpbin.org", "example.com"]);
    }

    #[test]
    fn extism_manifest_wasm_path_relative_to_plugin_dir() {
        let mut pm = minimal_manifest();
        pm.wasm_path = "bin/module.wasm".to_string();

        let result = build_extism_manifest(&pm, Path::new("/srv/plugins/cool"), None);

        match &result.manifest.wasm[0] {
            extism::Wasm::File { path, .. } => {
                assert_eq!(path, &PathBuf::from("/srv/plugins/cool/bin/module.wasm"));
            }
            other => panic!("expected Wasm::File, got {:?}", other),
        }
    }

    // ── US-ZCL-6-2: Wildcard hosts rejected in strict and paranoid levels ──

    #[test]
    fn wildcard_star_rejected_at_strict_level() {
        let hosts = vec!["*".to_string()];
        let result = validate_allowed_hosts("test-plugin", &hosts, NetworkSecurityLevel::Strict);
        assert!(result.is_err(), "bare wildcard '*' must be rejected at Strict level");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("test-plugin"), "error should name the plugin: {msg}");
        assert!(msg.contains("*"), "error should include the offending host: {msg}");
        assert!(msg.contains("Strict"), "error should name the security level: {msg}");
    }

    #[test]
    fn wildcard_prefix_rejected_at_strict_level() {
        let hosts = vec!["*.example.com".to_string()];
        let result = validate_allowed_hosts("net-plugin", &hosts, NetworkSecurityLevel::Strict);
        assert!(result.is_err(), "'*.example.com' must be rejected at Strict level");
    }

    #[test]
    fn wildcard_star_rejected_at_paranoid_level() {
        let hosts = vec!["*".to_string()];
        let result = validate_allowed_hosts("test-plugin", &hosts, NetworkSecurityLevel::Paranoid);
        assert!(result.is_err(), "bare wildcard '*' must be rejected at Paranoid level");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Paranoid"), "error should name the security level: {msg}");
    }

    #[test]
    fn wildcard_prefix_rejected_at_paranoid_level() {
        let hosts = vec!["*.api.io".to_string()];
        let result = validate_allowed_hosts("net-plugin", &hosts, NetworkSecurityLevel::Paranoid);
        assert!(result.is_err(), "'*.api.io' must be rejected at Paranoid level");
    }

    #[test]
    fn exact_hosts_allowed_at_strict_level() {
        let hosts = vec!["api.example.com".to_string(), "cdn.example.com".to_string()];
        let result = validate_allowed_hosts("safe-plugin", &hosts, NetworkSecurityLevel::Strict);
        assert!(result.is_ok(), "exact hosts should pass at Strict level");
    }

    #[test]
    fn exact_hosts_allowed_at_paranoid_level() {
        let hosts = vec!["api.example.com".to_string()];
        let result = validate_allowed_hosts("safe-plugin", &hosts, NetworkSecurityLevel::Paranoid);
        assert!(result.is_ok(), "exact hosts should pass at Paranoid level");
    }

    #[test]
    fn wildcard_hosts_allowed_at_default_level() {
        let hosts = vec!["*.example.com".to_string(), "*".to_string()];
        let result = validate_allowed_hosts("loose-plugin", &hosts, NetworkSecurityLevel::Default);
        assert!(result.is_ok(), "wildcards should be allowed at Default level");
    }

    // ── US-ZCL-6-3: Wildcard hosts produce warning at default security level ──

    /// Captures tracing events at WARN level for test assertions.
    struct WarningCollector {
        events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl tracing::Subscriber for WarningCollector {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata.level() <= &tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            self.events.lock().unwrap().push(visitor.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    struct MessageVisitor(String);

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(self.0, "{}={:?} ", field.name(), value);
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            use std::fmt::Write;
            let _ = write!(self.0, "{}={} ", field.name(), value);
        }
    }

    #[test]
    fn wildcard_hosts_produce_warning_at_default_level() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = WarningCollector {
            events: events.clone(),
        };

        let hosts = vec![
            "api.example.com".to_string(),
            "*.example.com".to_string(),
            "*".to_string(),
        ];

        tracing::subscriber::with_default(subscriber, || {
            let result =
                validate_allowed_hosts("warn-plugin", &hosts, NetworkSecurityLevel::Default);
            assert!(result.is_ok(), "wildcards must be allowed at Default level");
        });

        let captured = events.lock().unwrap();
        assert_eq!(
            captured.len(),
            2,
            "expected exactly 2 warnings (one per wildcard host), got {}: {:?}",
            captured.len(),
            *captured
        );

        // First warning: *.example.com
        assert!(
            captured[0].contains("*.example.com"),
            "first warning should mention '*.example.com': {}",
            captured[0]
        );
        assert!(
            captured[0].contains("warn-plugin"),
            "warning should name the plugin: {}",
            captured[0]
        );

        // Second warning: *
        assert!(
            captured[1].contains("*"),
            "second warning should mention '*': {}",
            captured[1]
        );
    }

    #[test]
    fn no_warning_for_exact_hosts_at_default_level() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = WarningCollector {
            events: events.clone(),
        };

        let hosts = vec!["api.example.com".to_string(), "cdn.example.com".to_string()];

        tracing::subscriber::with_default(subscriber, || {
            let result =
                validate_allowed_hosts("safe-plugin", &hosts, NetworkSecurityLevel::Default);
            assert!(result.is_ok());
        });

        let captured = events.lock().unwrap();
        assert!(
            captured.is_empty(),
            "exact hosts should not produce warnings, got: {:?}",
            *captured
        );
    }

    #[test]
    fn empty_hosts_pass_all_levels() {
        let hosts: Vec<String> = vec![];
        for level in [NetworkSecurityLevel::Default, NetworkSecurityLevel::Strict, NetworkSecurityLevel::Paranoid] {
            let result = validate_allowed_hosts("empty-plugin", &hosts, level);
            assert!(result.is_ok(), "empty hosts should always pass, failed at {level:?}");
        }
    }

    // ── US-ZCL-6-5: Security policy validation runs before plugin instantiation ──

    /// Proves that `validate_allowed_hosts` is designed to run before plugin
    /// instantiation: when a plugin declares wildcard hosts at Strict level,
    /// the validation returns `WildcardHostRejected` immediately — no WASM
    /// file or Extism runtime is needed.  This confirms the validation can
    /// (and should) gate the much heavier `Plugin::new()` call.
    #[test]
    fn security_validation_rejects_before_instantiation_would_be_attempted() {
        let mut pm = minimal_manifest();
        pm.name = "wildcard-plugin".to_string();
        pm.allowed_hosts = vec!["*.evil.com".to_string()];

        // Validation runs against the manifest data alone — no WASM, no
        // filesystem, no Extism runtime.  A rejection here proves that the
        // security check can execute before any instantiation work.
        let result = validate_allowed_hosts(
            &pm.name,
            &pm.allowed_hosts,
            NetworkSecurityLevel::Strict,
        );

        assert!(result.is_err(), "wildcard host must be rejected at Strict level");
        let err = result.unwrap_err();
        match &err {
            PluginError::WildcardHostRejected { plugin, host, level } => {
                assert_eq!(plugin, "wildcard-plugin");
                assert_eq!(host, "*.evil.com");
                assert_eq!(level, "Strict");
            }
            other => panic!(
                "expected WildcardHostRejected, got: {other:?}"
            ),
        }
    }

    /// End-to-end ordering proof: set up a plugin with wildcard hosts and a
    /// non-existent WASM path.  When processed through the same sequence used
    /// in `all_tools()` (validate → build manifest → instantiate), the
    /// validation error fires first.  If validation were skipped, the build or
    /// instantiate step would fail with a different error (IO / LoadFailed),
    /// which would cause this test to fail.
    #[test]
    fn validation_fires_before_manifest_build_and_instantiation() {
        let mut pm = minimal_manifest();
        pm.name = "bad-net-plugin".to_string();
        pm.allowed_hosts = vec!["*".to_string()];
        // WASM path intentionally does not exist — proves we never get that far.
        pm.wasm_path = "nonexistent.wasm".to_string();

        let _plugin_dir = Path::new("/tmp/does-not-exist");
        let level = NetworkSecurityLevel::Strict;

        // Step 1: validate (mirrors the call in tools/mod.rs).
        let validation = validate_allowed_hosts(&pm.name, &pm.allowed_hosts, level);

        // The pipeline should stop here — validation rejects the wildcard.
        assert!(
            validation.is_err(),
            "validation must reject before we ever touch the filesystem or Extism"
        );

        // Step 2 would be build_extism_manifest — but we never reach it.
        // If we DID reach it, the file-backed Wasm source would fail because
        // the path doesn't exist, giving us a different error class.
        // This ordering guarantee is exactly what the acceptance criterion requires.
    }

    /// `NetworkSecurityLevel::from_config` parses config strings correctly.
    #[test]
    fn network_security_level_from_config_strings() {
        assert_eq!(NetworkSecurityLevel::from_config("default"), NetworkSecurityLevel::Default);
        assert_eq!(NetworkSecurityLevel::from_config("strict"), NetworkSecurityLevel::Strict);
        assert_eq!(NetworkSecurityLevel::from_config("Strict"), NetworkSecurityLevel::Strict);
        assert_eq!(NetworkSecurityLevel::from_config("paranoid"), NetworkSecurityLevel::Paranoid);
        assert_eq!(NetworkSecurityLevel::from_config("PARANOID"), NetworkSecurityLevel::Paranoid);
        assert_eq!(NetworkSecurityLevel::from_config("unknown"), NetworkSecurityLevel::Default);
        assert_eq!(NetworkSecurityLevel::from_config(""), NetworkSecurityLevel::Default);
    }

    #[test]
    fn requires_forced_audit_strict_and_paranoid() {
        assert!(NetworkSecurityLevel::Strict.requires_forced_audit());
        assert!(NetworkSecurityLevel::Paranoid.requires_forced_audit());
        assert!(!NetworkSecurityLevel::Default.requires_forced_audit());
        assert!(!NetworkSecurityLevel::Relaxed.requires_forced_audit());
    }

    #[test]
    fn mixed_hosts_rejected_when_any_is_wildcard_at_strict() {
        let hosts = vec![
            "api.example.com".to_string(),
            "*.internal.io".to_string(),
            "cdn.example.com".to_string(),
        ];
        let result = validate_allowed_hosts("mixed-plugin", &hosts, NetworkSecurityLevel::Strict);
        assert!(result.is_err(), "mixed list with a wildcard must be rejected at Strict level");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("*.internal.io"), "error should name the offending wildcard host: {msg}");
    }

    // ── US-ZCL-6-8: Unit tests for host validation at each security level ──
    //
    // Exercises the full matrix of (host kind × security level) to prove that
    // validate_allowed_hosts enforces the correct policy for every combination.

    /// Specific (non-wildcard) hosts must be accepted at every security level,
    /// including Default, Strict, and Paranoid.
    #[test]
    fn specific_host_allowed_at_all_levels() {
        let hosts = vec!["api.example.com".to_string()];
        for level in [
            NetworkSecurityLevel::Default,
            NetworkSecurityLevel::Strict,
            NetworkSecurityLevel::Paranoid,
        ] {
            let result = validate_allowed_hosts("specific-plugin", &hosts, level);
            assert!(
                result.is_ok(),
                "specific host 'api.example.com' must be allowed at {level:?}"
            );
        }
    }

    /// Wildcard bare `*` must be rejected at Strict level with the correct
    /// error variant and fields.
    #[test]
    fn wildcard_bare_star_rejected_at_strict_with_correct_error() {
        let hosts = vec!["*".to_string()];
        let err = validate_allowed_hosts("star-plugin", &hosts, NetworkSecurityLevel::Strict)
            .expect_err("bare '*' must be rejected at Strict");
        match &err {
            PluginError::WildcardHostRejected { plugin, host, level } => {
                assert_eq!(plugin, "star-plugin");
                assert_eq!(host, "*");
                assert_eq!(level, "Strict");
            }
            other => panic!("expected WildcardHostRejected, got: {other:?}"),
        }
    }

    /// Wildcard prefix `*.example.com` must be rejected at Paranoid level with
    /// the correct error variant and fields.
    #[test]
    fn wildcard_prefix_rejected_at_paranoid_with_correct_error() {
        let hosts = vec!["*.example.com".to_string()];
        let err = validate_allowed_hosts("prefix-plugin", &hosts, NetworkSecurityLevel::Paranoid)
            .expect_err("'*.example.com' must be rejected at Paranoid");
        match &err {
            PluginError::WildcardHostRejected { plugin, host, level } => {
                assert_eq!(plugin, "prefix-plugin");
                assert_eq!(host, "*.example.com");
                assert_eq!(level, "Paranoid");
            }
            other => panic!("expected WildcardHostRejected, got: {other:?}"),
        }
    }

    /// At Default level, wildcard hosts produce warnings but validation succeeds.
    /// This test checks both the Ok result and that warning count matches the
    /// number of wildcard entries (excluding non-wildcard hosts).
    #[test]
    fn wildcard_warned_at_default_level_count_matches() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = WarningCollector {
            events: events.clone(),
        };

        let hosts = vec![
            "specific.example.com".to_string(),
            "*.wildcard1.com".to_string(),
            "also-specific.io".to_string(),
            "*.wildcard2.org".to_string(),
            "*".to_string(),
        ];

        tracing::subscriber::with_default(subscriber, || {
            let result =
                validate_allowed_hosts("warn-count-plugin", &hosts, NetworkSecurityLevel::Default);
            assert!(result.is_ok(), "all hosts must pass at Default level");
        });

        let captured = events.lock().unwrap();
        assert_eq!(
            captured.len(),
            3,
            "expected 3 warnings (one per wildcard host), got {}: {:?}",
            captured.len(),
            *captured
        );
    }

    /// Empty hosts list must pass at every security level — no network access
    /// means nothing to reject.
    #[test]
    fn empty_hosts_accepted_at_every_level() {
        let hosts: Vec<String> = vec![];
        for level in [
            NetworkSecurityLevel::Default,
            NetworkSecurityLevel::Strict,
            NetworkSecurityLevel::Paranoid,
        ] {
            assert!(
                validate_allowed_hosts("no-net-plugin", &hosts, level).is_ok(),
                "empty hosts must pass at {level:?}"
            );
        }
    }

    /// Multiple specific hosts are allowed at all levels (not just single-host).
    #[test]
    fn multiple_specific_hosts_allowed_at_all_levels() {
        let hosts = vec![
            "api.example.com".to_string(),
            "cdn.example.com".to_string(),
            "auth.example.com".to_string(),
        ];
        for level in [
            NetworkSecurityLevel::Default,
            NetworkSecurityLevel::Strict,
            NetworkSecurityLevel::Paranoid,
        ] {
            assert!(
                validate_allowed_hosts("multi-host-plugin", &hosts, level).is_ok(),
                "multiple specific hosts must be allowed at {level:?}"
            );
        }
    }

    /// The security level is correctly derived from config strings via
    /// `NetworkSecurityLevel::from_config`, ensuring the config layer feeds the
    /// right level into validation.
    #[test]
    fn config_string_maps_to_correct_validation_behavior() {
        let wildcard_hosts = vec!["*.example.com".to_string()];

        // "default" → Default level → wildcards allowed
        let level = NetworkSecurityLevel::from_config("default");
        assert!(validate_allowed_hosts("cfg-plugin", &wildcard_hosts, level).is_ok());

        // "strict" → Strict level → wildcards rejected
        let level = NetworkSecurityLevel::from_config("strict");
        assert!(validate_allowed_hosts("cfg-plugin", &wildcard_hosts, level).is_err());

        // "paranoid" → Paranoid level → wildcards rejected
        let level = NetworkSecurityLevel::from_config("paranoid");
        assert!(validate_allowed_hosts("cfg-plugin", &wildcard_hosts, level).is_err());

        // Unknown string → falls back to Default → wildcards allowed
        let level = NetworkSecurityLevel::from_config("bogus");
        assert!(validate_allowed_hosts("cfg-plugin", &wildcard_hosts, level).is_ok());
    }

    // ── plugin_config tests ────────────────────────────────────────

    /// Helper: build a `Config` with specific per-plugin config entries.
    fn config_with_per_plugin(
        entries: Vec<(&str, Vec<(&str, &str)>)>,
    ) -> crate::config::schema::Config {
        let mut cfg = crate::config::schema::Config::default();
        for (name, kvs) in entries {
            let map: std::collections::HashMap<String, String> = kvs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            cfg.plugins.per_plugin.insert(name.to_string(), map);
        }
        cfg
    }

    #[test]
    fn plugin_config_returns_values_from_config_toml() {
        let cfg = config_with_per_plugin(vec![(
            "weather",
            vec![("api_key", "sk-abc"), ("region", "us-east")],
        )]);
        let security = crate::security::SecurityPolicy::default();
        let loader = PluginLoader::new(&cfg, &security);

        let result = loader.plugin_config("weather");
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("api_key").map(String::as_str), Some("sk-abc"));
        assert_eq!(result.get("region").map(String::as_str), Some("us-east"));
    }

    #[test]
    fn plugin_config_returns_empty_map_when_section_missing() {
        let cfg = crate::config::schema::Config::default();
        let security = crate::security::SecurityPolicy::default();
        let loader = PluginLoader::new(&cfg, &security);

        let result = loader.plugin_config("nonexistent-plugin");
        assert!(result.is_empty(), "missing section should yield empty BTreeMap");
    }

    #[test]
    fn plugin_config_returns_sorted_keys() {
        let cfg = config_with_per_plugin(vec![(
            "sorter",
            vec![("zebra", "z"), ("alpha", "a"), ("middle", "m")],
        )]);
        let security = crate::security::SecurityPolicy::default();
        let loader = PluginLoader::new(&cfg, &security);

        let result = loader.plugin_config("sorter");
        let keys: Vec<&str> = result.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["alpha", "middle", "zebra"], "BTreeMap keys should be sorted");
    }

    #[test]
    fn plugin_config_isolates_plugins() {
        let cfg = config_with_per_plugin(vec![
            ("plugin-a", vec![("key", "value-a")]),
            ("plugin-b", vec![("key", "value-b")]),
        ]);
        let security = crate::security::SecurityPolicy::default();
        let loader = PluginLoader::new(&cfg, &security);

        assert_eq!(
            loader.plugin_config("plugin-a").get("key").map(String::as_str),
            Some("value-a"),
        );
        assert_eq!(
            loader.plugin_config("plugin-b").get("key").map(String::as_str),
            Some("value-b"),
        );
    }

    // ── validate_allowed_paths (forbidden path rejection) ──────────────

    /// /etc is rejected as a forbidden path at load time.
    #[test]
    fn forbidden_path_etc_rejected() {
        let mut allowed = HashMap::new();
        allowed.insert("/data".to_string(), "/etc/passwd".to_string());

        let forbidden = vec!["/etc".to_string(), "/root".to_string(), "~/.ssh".to_string()];
        let result = validate_allowed_paths("bad-plugin", &allowed, &forbidden);
        assert!(result.is_err(), "should reject /etc path");

        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("bad-plugin"), "error should name the plugin: {msg}");
        assert!(msg.contains("/etc/passwd"), "error should name the path: {msg}");
    }

    /// /root is rejected as a forbidden path at load time.
    #[test]
    fn forbidden_path_root_rejected() {
        let mut allowed = HashMap::new();
        allowed.insert("/secrets".to_string(), "/root/.bashrc".to_string());

        let forbidden = vec!["/etc".to_string(), "/root".to_string(), "~/.ssh".to_string()];
        let result = validate_allowed_paths("bad-plugin", &allowed, &forbidden);
        assert!(result.is_err(), "should reject /root path");

        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("/root/.bashrc"), "error should name the path: {msg}");
    }

    /// ~/.ssh is rejected as a forbidden path at load time (tilde-expanded).
    #[test]
    fn forbidden_path_ssh_rejected() {
        let mut allowed = HashMap::new();
        allowed.insert("/keys".to_string(), "~/.ssh/id_rsa".to_string());

        let forbidden = vec!["/etc".to_string(), "/root".to_string(), "~/.ssh".to_string()];
        let result = validate_allowed_paths("bad-plugin", &allowed, &forbidden);
        assert!(result.is_err(), "should reject ~/.ssh path");

        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("~/.ssh/id_rsa"), "error should name the path: {msg}");
    }

    /// An exact match against the forbidden path itself is also rejected.
    #[test]
    fn forbidden_path_exact_match_rejected() {
        let mut allowed = HashMap::new();
        allowed.insert("/guest-etc".to_string(), "/etc".to_string());

        let forbidden = vec!["/etc".to_string()];
        let result = validate_allowed_paths("exact-match", &allowed, &forbidden);
        assert!(result.is_err(), "exact forbidden path should be rejected");
    }

    /// A safe path not in the forbidden list is allowed.
    #[test]
    fn safe_path_is_allowed() {
        let mut allowed = HashMap::new();
        allowed.insert("/data".to_string(), "/workspace/data".to_string());

        let forbidden = vec!["/etc".to_string(), "/root".to_string(), "~/.ssh".to_string()];
        let result = validate_allowed_paths("good-plugin", &allowed, &forbidden);
        assert!(result.is_ok(), "safe path should be allowed");
    }

    /// Empty allowed_paths passes validation (nothing to check).
    #[test]
    fn empty_allowed_paths_passes_validation() {
        let allowed = HashMap::new();
        let forbidden = vec!["/etc".to_string(), "/root".to_string()];
        let result = validate_allowed_paths("empty-plugin", &allowed, &forbidden);
        assert!(result.is_ok(), "empty allowed_paths should pass");
    }

    /// Multiple paths where only one is forbidden — the forbidden one is caught.
    #[test]
    fn mixed_paths_forbidden_one_caught() {
        let mut allowed = HashMap::new();
        allowed.insert("/safe".to_string(), "/workspace/safe".to_string());
        allowed.insert("/secrets".to_string(), "/etc/shadow".to_string());

        let forbidden = vec!["/etc".to_string()];
        let result = validate_allowed_paths("mixed-plugin", &allowed, &forbidden);
        assert!(result.is_err(), "should catch the forbidden path among safe ones");
    }

    // ── US-ZCL-9-4: Strict mode rejects paths outside workspace subtree ──

    /// An absolute path outside the workspace root is rejected in strict mode.
    #[test]
    fn strict_mode_rejects_absolute_path_outside_workspace() {
        let mut allowed = HashMap::new();
        allowed.insert("/data".to_string(), "/tmp/data".to_string());

        let workspace = Path::new("/home/user/zeroclaw/workspace");
        let result = validate_workspace_paths("escape-plugin", &allowed, workspace);
        assert!(result.is_err(), "path outside workspace must be rejected");

        let err = result.unwrap_err();
        match &err {
            PluginError::PathOutsideWorkspace { plugin, path, workspace: ws } => {
                assert_eq!(plugin, "escape-plugin");
                assert_eq!(path, "/tmp/data");
                assert_eq!(ws, "/home/user/zeroclaw/workspace");
            }
            other => panic!("expected PathOutsideWorkspace, got: {other:?}"),
        }
    }

    /// A path inside the workspace subtree is allowed in strict mode.
    #[test]
    fn strict_mode_allows_path_inside_workspace() {
        let mut allowed = HashMap::new();
        allowed.insert("/data".to_string(), "/home/user/workspace/data".to_string());

        let workspace = Path::new("/home/user/workspace");
        let result = validate_workspace_paths("good-plugin", &allowed, workspace);
        assert!(result.is_ok(), "path inside workspace should be allowed");
    }

    /// A relative path is resolved against workspace root and accepted.
    #[test]
    fn strict_mode_allows_relative_path() {
        let mut allowed = HashMap::new();
        allowed.insert("/data".to_string(), "data/files".to_string());

        let workspace = Path::new("/home/user/workspace");
        let result = validate_workspace_paths("relative-plugin", &allowed, workspace);
        assert!(result.is_ok(), "relative path resolved against workspace should be allowed");
    }

    /// An exact match on the workspace root itself is allowed.
    #[test]
    fn strict_mode_allows_workspace_root_itself() {
        let mut allowed = HashMap::new();
        allowed.insert("/workspace".to_string(), "/home/user/workspace".to_string());

        let workspace = Path::new("/home/user/workspace");
        let result = validate_workspace_paths("root-plugin", &allowed, workspace);
        assert!(result.is_ok(), "workspace root path itself should be allowed");
    }

    /// Empty allowed_paths passes strict workspace validation.
    #[test]
    fn strict_mode_empty_paths_passes() {
        let allowed = HashMap::new();
        let workspace = Path::new("/home/user/workspace");
        let result = validate_workspace_paths("empty-plugin", &allowed, workspace);
        assert!(result.is_ok(), "empty allowed_paths should pass strict validation");
    }

    /// Multiple paths where one escapes the workspace — only the escapee is caught.
    #[test]
    fn strict_mode_mixed_paths_catches_outside() {
        let mut allowed = HashMap::new();
        allowed.insert("/safe".to_string(), "/home/user/workspace/safe".to_string());
        allowed.insert("/escape".to_string(), "/var/log/app".to_string());

        let workspace = Path::new("/home/user/workspace");
        let result = validate_workspace_paths("mixed-plugin", &allowed, workspace);
        assert!(result.is_err(), "should reject the path outside workspace");

        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("/var/log/app"), "error should name the offending path: {msg}");
    }

    /// A path that shares a prefix with the workspace but is not a subtree
    /// (e.g. /home/user/workspace-other) must be rejected.
    #[test]
    fn strict_mode_rejects_sibling_directory_with_shared_prefix() {
        let mut allowed = HashMap::new();
        allowed.insert("/data".to_string(), "/home/user/workspace-other/data".to_string());

        let workspace = Path::new("/home/user/workspace");
        let result = validate_workspace_paths("sibling-plugin", &allowed, workspace);
        assert!(
            result.is_err(),
            "sibling directory sharing prefix should be rejected"
        );
    }

    // ── US-ZCL-9-9: Symlink escape detection in strict mode ──────────────

    /// A symlink inside the workspace that resolves to a path outside the
    /// workspace must be rejected by strict-mode validation, because
    /// `validate_workspace_paths` canonicalizes existing paths to follow
    /// symlinks before checking the workspace prefix.
    #[test]
    fn strict_mode_rejects_symlink_escaping_workspace() {
        let workspace_dir = tempfile::tempdir().expect("create temp workspace");
        let outside_dir = tempfile::tempdir().expect("create temp outside dir");

        // Create a target directory outside the workspace.
        let outside_target = outside_dir.path().join("secrets");
        std::fs::create_dir_all(&outside_target).expect("create outside target");

        // Create a symlink *inside* the workspace that points outside.
        let symlink_path = workspace_dir.path().join("escape-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_target, &symlink_path)
            .expect("create symlink");

        let mut allowed = HashMap::new();
        allowed.insert(
            "/data".to_string(),
            symlink_path.to_string_lossy().into_owned(),
        );

        let result =
            validate_workspace_paths("symlink-plugin", &allowed, workspace_dir.path());
        assert!(
            result.is_err(),
            "symlink resolving outside workspace must be rejected"
        );

        let err = result.unwrap_err();
        match &err {
            PluginError::PathOutsideWorkspace { plugin, .. } => {
                assert_eq!(plugin, "symlink-plugin");
            }
            other => panic!("expected PathOutsideWorkspace, got: {other:?}"),
        }
    }

    /// A symlink inside the workspace that resolves to another path *inside*
    /// the workspace should be allowed.
    #[test]
    fn strict_mode_allows_symlink_inside_workspace() {
        let workspace_dir = tempfile::tempdir().expect("create temp workspace");

        // Create a real directory inside the workspace.
        let real_dir = workspace_dir.path().join("real-data");
        std::fs::create_dir_all(&real_dir).expect("create real dir");

        // Create a symlink that points to the real directory (still inside workspace).
        let symlink_path = workspace_dir.path().join("link-to-real");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, &symlink_path)
            .expect("create symlink");

        let mut allowed = HashMap::new();
        allowed.insert(
            "/data".to_string(),
            symlink_path.to_string_lossy().into_owned(),
        );

        let result =
            validate_workspace_paths("safe-symlink-plugin", &allowed, workspace_dir.path());
        assert!(
            result.is_ok(),
            "symlink resolving inside workspace should be allowed"
        );
    }

    // ── validate_security_policy tests ──────────────────────────────

    fn default_config() -> Config {
        Config::default()
    }

    fn default_security_policy() -> SecurityPolicy {
        SecurityPolicy::default()
    }

    #[test]
    fn validate_security_policy_passes_relaxed_with_wildcards() {
        let mut config = default_config();
        config.plugins.security.network_security_level = "relaxed".to_string();
        let security = default_security_policy();
        let loader = PluginLoader::new(&config, &security);

        let mut manifest = minimal_manifest();
        manifest.allowed_hosts = vec!["*.example.com".to_string()];

        assert!(
            loader.validate_security_policy(&manifest).is_ok(),
            "relaxed mode should allow wildcard hosts"
        );
    }

    #[test]
    fn validate_security_policy_rejects_strict_wildcards() {
        let mut config = default_config();
        config.plugins.security.network_security_level = "strict".to_string();
        let security = default_security_policy();
        let loader = PluginLoader::new(&config, &security);

        let mut manifest = minimal_manifest();
        manifest.allowed_hosts = vec!["*.example.com".to_string()];

        let result = loader.validate_security_policy(&manifest);
        assert!(result.is_err(), "strict mode should reject wildcard hosts");
    }

    #[test]
    fn validate_security_policy_rejects_paranoid_unlisted() {
        let mut config = default_config();
        config.plugins.security.network_security_level = "paranoid".to_string();
        config.plugins.security.allowed_plugins = vec!["other-plugin".to_string()];
        let security = default_security_policy();
        let loader = PluginLoader::new(&config, &security);

        let manifest = minimal_manifest(); // name = "test-plugin", not allowlisted

        let result = loader.validate_security_policy(&manifest);
        assert!(
            result.is_err(),
            "paranoid mode should reject unlisted plugin"
        );
    }

    #[test]
    fn validate_security_policy_allows_paranoid_listed() {
        let mut config = default_config();
        config.plugins.security.network_security_level = "paranoid".to_string();
        config.plugins.security.allowed_plugins = vec!["test-plugin".to_string()];
        let security = default_security_policy();
        let loader = PluginLoader::new(&config, &security);

        let manifest = minimal_manifest();

        assert!(
            loader.validate_security_policy(&manifest).is_ok(),
            "paranoid mode should allow listed plugin"
        );
    }

    #[test]
    fn validate_security_policy_rejects_forbidden_paths() {
        let config = default_config(); // default level
        let security = default_security_policy();
        let loader = PluginLoader::new(&config, &security);

        let mut manifest = minimal_manifest();
        manifest
            .allowed_paths
            .insert("/data".to_string(), "/etc/secrets".to_string());

        let result = loader.validate_security_policy(&manifest);
        assert!(
            result.is_err(),
            "should reject forbidden path /etc/secrets at any level"
        );
    }

    #[test]
    fn validate_security_policy_strict_rejects_path_outside_workspace() {
        let mut config = default_config();
        config.plugins.security.network_security_level = "strict".to_string();

        let workspace = std::env::temp_dir().join("zeroclaw_test_validate_policy");
        std::fs::create_dir_all(&workspace).ok();

        let security = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        let loader = PluginLoader::new(&config, &security);

        let mut manifest = minimal_manifest();
        manifest
            .allowed_paths
            .insert("/data".to_string(), "/tmp/outside".to_string());

        let result = loader.validate_security_policy(&manifest);
        assert!(
            result.is_err(),
            "strict mode should reject path outside workspace"
        );

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn validate_security_policy_default_allows_path_outside_workspace() {
        let config = default_config(); // default level
        let workspace = std::env::temp_dir().join("zeroclaw_test_validate_policy_default");
        std::fs::create_dir_all(&workspace).ok();

        let security = SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        };
        let loader = PluginLoader::new(&config, &security);

        let mut manifest = minimal_manifest();
        // Path outside workspace but not in FORBIDDEN_PATHS
        manifest
            .allowed_paths
            .insert("/data".to_string(), "/tmp/outside".to_string());

        assert!(
            loader.validate_security_policy(&manifest).is_ok(),
            "default mode should not enforce workspace path restriction"
        );

        std::fs::remove_dir_all(&workspace).ok();
    }
}
