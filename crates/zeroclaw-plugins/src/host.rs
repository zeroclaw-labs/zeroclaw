//! Plugin host: discovery, loading, lifecycle management.

use super::error::PluginError;
use super::signature::{self, SignatureMode};
use super::{PluginCapability, PluginInfo, PluginManifest};
use crate::config::validate_manifest_config;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Subdirectory inside a skill-capable plugin that holds individual skills.
const SKILLS_SUBDIR: &str = "skills";

/// Manages the lifecycle of WASM plugins.
pub struct PluginHost {
    plugins_dir: PathBuf,
    loaded: HashMap<String, LoadedPlugin>,
    signature_mode: SignatureMode,
    trusted_publisher_keys: Vec<String>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    /// The manifest text `manifest` was parsed from and its signature was
    /// checked against.
    manifest_toml: String,
    plugin_dir: PathBuf,
    /// Exact executable bytes accepted with this manifest. `None` for
    /// skill-only plugins.
    component: Option<AdmittedComponent>,
}

/// Exact executable bytes that passed package confinement and digest policy.
///
/// Runtime adapters consume this artifact instead of reopening a manifest
/// path, so compilation uses the payload generation that admission retained.
#[derive(Clone)]
pub struct AdmittedComponent {
    bytes: Arc<[u8]>,
}

impl AdmittedComponent {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::from(bytes),
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    // Gated exactly like its callers: every use lives in a `tests` module
    // inside a `plugins-wasmtime` module, so under default features the
    // helper would otherwise be dead code and fail the deny-warnings check.
    #[cfg(all(test, feature = "plugins-wasmtime"))]
    pub(crate) fn test_component(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }
}

/// A source that passed admission and is ready to install: see
/// [`PluginHost::admit_source`]. It carries the exact manifest and component
/// bytes admission read, so a caller can load-check them and
/// [`PluginHost::install_admitted`] then persists those same bytes.
pub struct AdmittedSource {
    manifest: PluginManifest,
    manifest_toml: String,
    source_dir: PathBuf,
    component: Option<AdmittedComponent>,
}

impl std::fmt::Debug for AdmittedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmittedSource")
            .field("plugin", &self.manifest.name)
            .field(
                "component_bytes",
                &self.component.as_ref().map(|c| c.bytes().len()),
            )
            .finish_non_exhaustive()
    }
}

impl AdmittedSource {
    /// The admitted manifest.
    #[must_use]
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// The manifest text admission parsed and checked against the signature
    /// policy. [`PluginHost::install_admitted`] writes these bytes, so a
    /// signature checked against them is checked against what gets installed.
    #[must_use]
    pub fn manifest_toml(&self) -> &str {
        &self.manifest_toml
    }

    /// The admitted component to load-check, or `None` for a package that
    /// ships no WASM. These are the bytes [`PluginHost::install_admitted`]
    /// writes, so a check against them is a check of what gets installed.
    #[must_use]
    pub fn component(&self) -> Option<&AdmittedComponent> {
        self.component.as_ref()
    }
}

impl PluginHost {
    /// Create a new plugin host rooted at `workspace_dir`, scanning its
    /// `plugins/` subdirectory.
    pub fn new(workspace_dir: &Path) -> Result<Self, PluginError> {
        Self::with_security(workspace_dir, SignatureMode::Disabled, Vec::new())
    }

    /// Create a host rooted at `workspace_dir` (scanning `workspace_dir/plugins`)
    /// with signature verification settings.
    pub fn with_security(
        workspace_dir: &Path,
        signature_mode: SignatureMode,
        trusted_publisher_keys: Vec<String>,
    ) -> Result<Self, PluginError> {
        Self::from_plugins_dir_with_security(
            &workspace_dir.join("plugins"),
            signature_mode,
            trusted_publisher_keys,
        )
    }

    /// Create a host that scans `plugins_dir` directly (no `plugins/` suffix is
    /// appended). Use this when the caller already holds the fully resolved
    /// plugin directory, e.g. `PluginsConfig::resolved_plugins_dir()`.
    pub fn from_plugins_dir(plugins_dir: &Path) -> Result<Self, PluginError> {
        Self::from_plugins_dir_with_security(plugins_dir, SignatureMode::Disabled, Vec::new())
    }

    /// [`Self::from_plugins_dir`] with signature verification settings.
    pub fn from_plugins_dir_with_security(
        plugins_dir: &Path,
        signature_mode: SignatureMode,
        trusted_publisher_keys: Vec<String>,
    ) -> Result<Self, PluginError> {
        if !plugins_dir.exists() {
            std::fs::create_dir_all(plugins_dir)?;
        }

        let mut host = Self {
            plugins_dir: plugins_dir.to_path_buf(),
            loaded: HashMap::new(),
            signature_mode,
            trusted_publisher_keys,
        };

        host.discover()?;
        Ok(host)
    }

    pub fn parse_signature_mode(mode: &str) -> Option<SignatureMode> {
        match mode.to_lowercase().as_str() {
            "strict" => Some(SignatureMode::Strict),
            "permissive" => Some(SignatureMode::Permissive),
            "disabled" => Some(SignatureMode::Disabled),
            _ => None,
        }
    }

    #[must_use]
    pub fn resolve_signature_mode(mode: &str) -> SignatureMode {
        Self::parse_signature_mode(mode).unwrap_or_else(|| {
            let span = ::zeroclaw_log::__private::tracing::info_span!(
                target: "zeroclaw_log_internal_attribution",
                "zeroclaw_attribution",
                zc_role_family = %::zeroclaw_api::attribution::Role::System.family_str(),
                zc_role_type = "",
                zc_attribution_field = %::zeroclaw_api::attribution::Role::System
                    .attribution_field()
                    .unwrap_or(""),
                zc_composite_prefix = "",
                zc_default_category = %::zeroclaw_api::attribution::Role::System.default_category(),
                zc_alias = "plugins",
            );
            span.in_scope(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "signature_mode": mode })),
                    "Unrecognized plugins.security.signature_mode; failing safe to strict"
                );
            });
            SignatureMode::Strict
        })
    }

    /// Discover plugins in the plugins directory.
    fn discover(&mut self) -> Result<(), PluginError> {
        if !self.plugins_dir.exists() {
            return Ok(());
        }

        let mut ambiguous_packages = HashSet::new();
        let entries = std::fs::read_dir(&self.plugins_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            // A package root is an admission boundary. Do not follow a
            // directory symlink supplied at the discovery root: it would make
            // an external package appear local before its manifest and payload
            // confinement checks begin.
            // Dot-prefixed directories are never packages: they include the
            // staging directories `install_admitted` builds a package in.
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if entry.file_type()?.is_dir() && !hidden {
                let manifest_path = path.join("manifest.toml");
                if manifest_path.exists()
                    && let Ok((manifest, manifest_toml)) = self.load_manifest(&manifest_path)
                {
                    if let Err(e) = validate_manifest_shape(&manifest, &path) {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to invalid manifest shape");
                        continue;
                    }

                    // Verify the manifest, then retain the executable bytes that
                    // match its declared package policy.
                    match self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest) {
                        Ok(()) => {
                            if let Err(e) = validate_manifest_config(&manifest) {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to invalid config schema");
                                continue;
                            }
                            if ambiguous_packages.contains(&manifest.name) {
                                continue;
                            }
                            if self.loaded.remove(&manifest.name).is_some() {
                                ambiguous_packages.insert(manifest.name.clone());
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Load
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "plugin": manifest.name,
                                        })
                                    ),
                                    "rejecting ambiguous duplicate plugin package"
                                );
                                continue;
                            }
                            let component = match admit_component(&path, &manifest) {
                                Ok(component) => component,
                                Err(e) => {
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to executable artifact admission failure");
                                    continue;
                                }
                            };
                            self.loaded.insert(
                                manifest.name.clone(),
                                LoadedPlugin {
                                    manifest,
                                    manifest_toml,
                                    plugin_dir: path.clone(),
                                    component,
                                },
                            );
                        }
                        Err(e) => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to signature verification failure");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn load_manifest(&self, path: &Path) -> Result<(PluginManifest, String), PluginError> {
        let content = std::fs::read_to_string(path)?;
        let manifest: PluginManifest = toml::from_str(&content)?;
        Ok((manifest, content))
    }

    /// Verify a plugin's signature against configured policy.
    fn verify_plugin_signature(
        &self,
        name: &str,
        manifest_toml: &str,
        manifest: &PluginManifest,
    ) -> Result<(), PluginError> {
        signature::enforce_signature_policy(
            name,
            manifest_toml,
            manifest.signature.as_deref(),
            manifest.publisher_key.as_deref(),
            &self.trusted_publisher_keys,
            self.signature_mode,
        )?;
        if self.signature_mode == SignatureMode::Strict
            && manifest.wasm_path.is_some()
            && manifest.wasm_sha256.is_none()
        {
            return Err(PluginError::PayloadDigestRequired(name.to_string()));
        }
        Ok(())
    }

    /// List all discovered plugins.
    ///
    /// Sorted by package name so the listing is stable across runs: the loaded
    /// set is a hash map, and an operator diffing two `plugin list` outputs, or a
    /// script reading the verified rows, needs the order to mean nothing.
    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        let mut plugins: Vec<PluginInfo> =
            self.loaded.values().map(plugin_info_from_loaded).collect();
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        plugins
    }

    /// Get info about a specific plugin.
    pub fn get_plugin(&self, name: &str) -> Option<PluginInfo> {
        self.loaded.get(name).map(plugin_info_from_loaded)
    }

    /// Return the admitted manifest that owns a plugin's runtime contract.
    #[must_use]
    pub fn manifest(&self, name: &str) -> Option<&PluginManifest> {
        self.loaded.get(name).map(|plugin| &plugin.manifest)
    }

    /// The manifest text an installed plugin's [`Self::manifest`] was parsed
    /// from and its signature was checked against: the bytes this host
    /// loaded, not the file as it may read now.
    #[must_use]
    pub fn manifest_toml(&self, name: &str) -> Option<&str> {
        self.loaded
            .get(name)
            .map(|plugin| plugin.manifest_toml.as_str())
    }

    /// The exact component bytes admitted for an installed plugin: the bytes
    /// the daemon compiles, so a load check against them checks what will run.
    /// `None` for an unknown plugin and for a skill-only plugin that ships no
    /// WASM.
    pub fn admitted_component(&self, name: &str) -> Option<&AdmittedComponent> {
        self.loaded
            .get(name)
            .and_then(|plugin| plugin.component.as_ref())
    }

    /// Install a plugin from a directory path. Returns the installed
    /// plugin's manifest name so callers can key follow-up work (config
    /// seeding, messaging) off the canonical name rather than the source path.
    ///
    /// This is [`Self::admit_source`] followed by [`Self::install_admitted`].
    /// A caller that wants to load-check the component in between (the CLI's
    /// install-time verification) calls the two halves itself; either way,
    /// the bytes that were admitted are the bytes that get installed.
    pub fn install(&mut self, source: &str) -> Result<String, PluginError> {
        let admitted = self.admit_source(source)?;
        self.install_admitted(admitted)
    }

    /// Admit a source without installing it.
    ///
    /// Parses and signature-checks the manifest, validates its shape and
    /// config, refuses a name the host has already loaded, and reads the
    /// component once through the same confined, size-bounded, digest-checked
    /// admission the host applies to installed plugins. Everything
    /// [`Self::install_admitted`] persists comes from the returned
    /// [`AdmittedSource`] and never from `source` again, so whatever a caller
    /// verifies between the two calls is, byte for byte, what gets installed.
    ///
    /// The duplicate-name check runs before the component is read, so a
    /// package the host would refuse anyway never reaches the loader.
    pub fn admit_source(&self, source: &str) -> Result<AdmittedSource, PluginError> {
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

        let (manifest, manifest_toml) = self.load_manifest(&manifest_path)?;
        let source_dir = manifest_path
            .parent()
            .ok_or_else(|| PluginError::InvalidManifest("no parent directory".into()))?
            .to_path_buf();

        validate_manifest_shape(&manifest, &source_dir)?;

        // Refuse a duplicate before the signature check and before the
        // component is read: nothing downstream may run for a package the host
        // has already decided not to install.
        if self.loaded.contains_key(&manifest.name) {
            return Err(PluginError::AlreadyLoaded(manifest.name));
        }

        // Admit the exact manifest and payload generations before installation.
        self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest)?;
        validate_manifest_config(&manifest)?;
        let component = admit_component(&source_dir, &manifest)?;

        Ok(AdmittedSource {
            manifest,
            manifest_toml,
            source_dir,
            component,
        })
    }

    /// Install a source admitted by [`Self::admit_source`].
    ///
    /// Persists the manifest bytes that were parsed and signature-checked and
    /// the component bytes that were admitted, so the file a verifier checked
    /// is the file the daemon will load. The `skills/` subtree of a
    /// skill-capable package is copied from the source directory as before; it
    /// is data, not executable code.
    pub fn install_admitted(&mut self, admitted: AdmittedSource) -> Result<String, PluginError> {
        let AdmittedSource {
            manifest,
            manifest_toml,
            source_dir,
            component,
        } = admitted;

        // Re-checked here as well: the host may have loaded the name between
        // admission and installation.
        if self.loaded.contains_key(&manifest.name) {
            return Err(PluginError::AlreadyLoaded(manifest.name));
        }

        let dest_dir = self.plugins_dir.join(&manifest.name);
        if dest_dir.exists() {
            return Err(PluginError::AlreadyLoaded(manifest.name));
        }

        // Build the package in a staging directory and rename it into place,
        // so a failed write (disk full, an unreadable skill file, an
        // interrupted process) never leaves a half-written package under the
        // real name: that would block every retry with `AlreadyLoaded` while
        // discovery skips it, leaving nothing `plugin remove` can find.
        // Discovery ignores dot-prefixed directories, so a staging directory
        // stranded by a crash is never loaded either.
        std::fs::create_dir_all(&self.plugins_dir)?;
        let staging = self.plugins_dir.join(format!(
            ".{}.installing-{}",
            manifest.name,
            std::process::id()
        ));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        let staged = write_package(
            &staging,
            &manifest,
            &manifest_toml,
            &source_dir,
            component.as_ref(),
        )
        .and_then(|()| std::fs::rename(&staging, &dest_dir).map_err(PluginError::from));
        if let Err(e) = staged {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }

        let installed_name = manifest.name.clone();
        self.loaded.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
                manifest_toml,
                plugin_dir: dest_dir,
                component,
            },
        );

        Ok(installed_name)
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

    /// Get tool-capable plugins with their admitted executable bytes.
    pub fn tool_plugin_details(&self) -> Vec<(&PluginManifest, &AdmittedComponent)> {
        self.executable_plugin_details(PluginCapability::Tool)
    }

    /// Get channel-capable plugins.
    pub fn channel_plugins(&self) -> Vec<&PluginManifest> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Channel))
            .map(|p| &p.manifest)
            .collect()
    }

    pub fn channel_plugin_details(&self) -> Vec<(&PluginManifest, &AdmittedComponent)> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Channel))
            .filter_map(|p| {
                p.component
                    .as_ref()
                    .map(|component| (&p.manifest, component))
            })
            .collect()
    }

    fn executable_plugin_details(
        &self,
        capability: PluginCapability,
    ) -> Vec<(&PluginManifest, &AdmittedComponent)> {
        self.loaded
            .values()
            .filter(|plugin| plugin.manifest.capabilities.contains(&capability))
            .filter_map(|plugin| {
                plugin
                    .component
                    .as_ref()
                    .map(|component| (&plugin.manifest, component))
            })
            .collect()
    }

    /// Get skill-capable plugins.
    pub fn skill_plugins(&self) -> Vec<&PluginManifest> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Skill))
            .map(|p| &p.manifest)
            .collect()
    }

    pub fn skill_plugin_details(&self) -> Vec<(&PluginManifest, PathBuf)> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Skill))
            .filter_map(|p| {
                let skills_dir = p.plugin_dir.join(SKILLS_SUBDIR);
                if skills_dir.is_dir() {
                    Some((&p.manifest, skills_dir))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }
}

fn plugin_info_from_loaded(p: &LoadedPlugin) -> PluginInfo {
    let loaded = match &p.component {
        Some(_) => true,
        // Skill-only plugins are "loaded" if their skills/ subtree exists.
        None => p.plugin_dir.join(SKILLS_SUBDIR).is_dir(),
    };
    PluginInfo {
        name: p.manifest.name.clone(),
        version: p.manifest.version.clone(),
        description: p.manifest.description.clone(),
        capabilities: p.manifest.capabilities.clone(),
        permissions: p.manifest.permissions.clone(),
        wasm_path: p
            .manifest
            .wasm_path
            .as_deref()
            .map(|relative| p.plugin_dir.join(relative)),
        loaded,
    }
}

fn admit_component(
    plugin_dir: &Path,
    manifest: &PluginManifest,
) -> Result<Option<AdmittedComponent>, PluginError> {
    manifest
        .wasm_path
        .as_deref()
        .map(|relative| {
            let confined = resolve_confined_wasm_path(plugin_dir, relative)?;
            let bytes = read_stable_file(&confined)?;
            if let Some(expected) = manifest.wasm_sha256.as_deref() {
                signature::verify_payload_digest(&bytes, expected)?;
            }
            Ok(AdmittedComponent::new(bytes))
        })
        .transpose()
}

/// A payload that passed package confinement: the canonical package root, the
/// payload's path below it (normal components only), and, on Unix, the root
/// directory held open since confinement.
///
/// On Unix that open root is the source of truth for every later open: the
/// payload is reached by walking `relative` down from it, never by resolving an
/// absolute pathname again. `root` is kept for messages and to refuse a package
/// root that was moved or replaced after confinement. Other platforms carry the
/// root's identity instead and check by pathname; see [`read_stable_file`].
pub(crate) struct ConfinedPayload {
    root: PathBuf,
    relative: PathBuf,
    #[cfg(unix)]
    root_dir: std::os::fd::OwnedFd,
    #[cfg(not(unix))]
    root_handle: same_file::Handle,
}

/// Resolve a manifest executable without allowing traversal or symlink
/// indirection outside the package.
///
/// On Unix the canonical package root is opened once, here, and the returned
/// [`ConfinedPayload`] holds that handle; the path below it is checked with
/// no-follow opens relative to it, so a component that is a symlink is refused
/// rather than followed. Elsewhere the checks go by pathname.
fn resolve_confined_wasm_path(
    plugin_dir: &Path,
    relative: &str,
) -> Result<ConfinedPayload, PluginError> {
    let requested = Path::new(relative);
    // Normal components only: a `.` names nothing, and `..`, root, and prefix
    // components are refused below.
    let relative: PathBuf = requested
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .collect();
    if relative.as_os_str().is_empty()
        || requested.is_absolute()
        || requested.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(PluginError::InvalidManifest(format!(
            "wasm_path must be a confined relative path (got {})",
            requested.display()
        )));
    }

    confine_payload(std::fs::canonicalize(plugin_dir)?, relative)
}

/// Open the canonical package root and check, relative to that handle and
/// without following a symlink, that `relative` names a regular file below it.
#[cfg(unix)]
fn confine_payload(root: PathBuf, relative: PathBuf) -> Result<ConfinedPayload, PluginError> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;

    let root_dir = rustix::fs::openat(
        rustix::fs::CWD,
        root.as_path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| match errno {
        // `root` was canonical a moment ago, so a symlink or non-directory
        // there now is a replaced root.
        Errno::LOOP | Errno::NOTDIR => PluginError::InvalidManifest(format!(
            "plugin package root changed during confinement: {}",
            root.display()
        )),
        other => PluginError::Io(other.into()),
    })?;
    #[cfg(test)]
    payload_step(PayloadStep::AfterRootOpen);

    let confined = ConfinedPayload {
        root,
        relative,
        root_dir,
    };
    let missing = |walked: &Path| {
        PluginError::NotFound(format!(
            "WASM file not found: {}",
            confined.root.join(walked).display()
        ))
    };
    let (parent, leaf) = confined.open_payload_parent(|errno, walked| match errno {
        Errno::NOENT => missing(walked),
        // A symlink or non-directory where a directory is expected fails the
        // no-follow directory open with ENOTDIR (ELOOP on older Linux kernels).
        Errno::LOOP | Errno::NOTDIR => PluginError::InvalidManifest(format!(
            "wasm_path contains a symlink or non-directory component: {}",
            confined.relative.display()
        )),
        other => PluginError::Io(other.into()),
    })?;
    let leaf_stat =
        rustix::fs::statat(&parent, leaf, AtFlags::SYMLINK_NOFOLLOW).map_err(|errno| {
            if errno == Errno::NOENT {
                missing(&confined.relative)
            } else {
                PluginError::Io(errno.into())
            }
        })?;
    match FileType::from_raw_mode(leaf_stat.st_mode) {
        FileType::RegularFile => Ok(confined),
        FileType::Symlink => Err(PluginError::InvalidManifest(format!(
            "wasm_path contains a symlink: {}",
            confined.relative.display()
        ))),
        _ => Err(PluginError::InvalidManifest(format!(
            "WASM payload is not a regular file: {}",
            confined.root.join(&confined.relative).display()
        ))),
    }
}

/// Check by pathname that `relative` names a regular file below the canonical
/// root with no symlink on the way, and record the root's identity.
#[cfg(not(unix))]
fn confine_payload(root: PathBuf, relative: PathBuf) -> Result<ConfinedPayload, PluginError> {
    let root_handle = same_file::Handle::from_path(&root)?;
    let mut candidate = root.clone();
    for segment in &relative {
        candidate.push(segment);
        let metadata = std::fs::symlink_metadata(&candidate).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                PluginError::NotFound(format!("WASM file not found: {}", candidate.display()))
            } else {
                PluginError::Io(error)
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(PluginError::InvalidManifest(format!(
                "wasm_path contains a symlink: {}",
                relative.display()
            )));
        }
    }

    if !std::fs::metadata(&candidate)?.is_file() {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload is not a regular file: {}",
            candidate.display()
        )));
    }
    let resolved = std::fs::canonicalize(&candidate)?;
    if !resolved.starts_with(&root) || resolved != candidate {
        return Err(PluginError::InvalidManifest(format!(
            "wasm_path escapes plugin directory: {}",
            relative.display()
        )));
    }
    Ok(ConfinedPayload {
        root,
        relative,
        root_handle,
    })
}

#[cfg(unix)]
impl ConfinedPayload {
    /// Walk the directories of `relative` down from the retained root, opening
    /// each relative to the one before without following a symlink, and return
    /// the payload's parent directory and file name. `on_error` maps a failed
    /// open, given the part of `relative` walked so far.
    fn open_payload_parent(
        &self,
        on_error: impl Fn(rustix::io::Errno, &Path) -> PluginError,
    ) -> Result<(std::os::fd::OwnedFd, &std::ffi::OsStr), PluginError> {
        use rustix::fs::{Mode, OFlags};

        let leaf = self.relative.file_name().ok_or_else(|| {
            PluginError::InvalidManifest(format!(
                "wasm_path must name a file (got {})",
                self.relative.display()
            ))
        })?;
        let mut dir = self.root_dir.try_clone()?;
        let mut walked = PathBuf::new();
        for segment in self.relative.parent().into_iter().flat_map(Path::iter) {
            walked.push(segment);
            dir = rustix::fs::openat(
                &dir,
                segment,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|errno| on_error(errno, &walked))?;
            #[cfg(test)]
            payload_step(PayloadStep::AfterDir(walked.components().count()));
        }
        Ok((dir, leaf))
    }
}

/// Largest executable payload admission will read into memory.
///
/// Discovery and install read a whole payload before its digest is verified or
/// it is compiled, so without a bound an oversized file is fully retained
/// before anything has a chance to reject it. 64 MiB clears real WASM
/// components — including debug-info builds — by a wide margin while keeping
/// a single malformed or hostile package from exhausting memory.
const MAX_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;

/// Read a confined payload, bounded by [`MAX_COMPONENT_BYTES`].
///
/// On Unix every open descends from the package root handle retained at
/// confinement and never follows a symlink, so every object read is reached
/// through real directories below that root as they stand at each open: a
/// symlink or non-directory the walk meets is refused, a directory moved away
/// after the walk passed it is never consulted, and a directory renamed into
/// place before the walk reaches it is traversed like any other directory
/// below the root. The root pathname is looked at only to refuse a package
/// whose root no longer names the directory the payload came from. The payload
/// is opened non-blocking and must be a regular file, so a FIFO or device fails
/// at once.
///
/// Other platforms open the absolute pathname and then compare the root
/// identity, each component's link status, and the opened file's identity with
/// what the pathname names. That refuses a replacement still in place when the
/// checks run, but the lookup is not atomic, and the open itself does not guard
/// against a FIFO.
pub(crate) fn read_stable_file(confined: &ConfinedPayload) -> Result<Vec<u8>, PluginError> {
    let path = confined.root.join(&confined.relative);
    let file = open_confined_payload(confined, &path)?;

    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload is not a regular file: {}",
            path.display()
        )));
    }

    if opened_metadata.len() > MAX_COMPONENT_BYTES {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload exceeds the {MAX_COMPONENT_BYTES}-byte admission limit: {} is {} bytes",
            path.display(),
            opened_metadata.len()
        )));
    }

    // Bound the read, not just the stat above: the payload can grow between the
    // size check and this read. Taking one byte past the limit makes an
    // oversized payload detectable without retaining more than that.
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_metadata.len().min(MAX_COMPONENT_BYTES)).unwrap_or(0),
    );
    file.take(MAX_COMPONENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_COMPONENT_BYTES {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload grew past the {MAX_COMPONENT_BYTES}-byte admission limit while being read: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Open the payload from the root handle retained at confinement: walk down
/// without following a symlink, open the payload itself non-blocking, then
/// check that the root pathname still names the directory it was read from.
/// `path` is for messages only.
#[cfg(unix)]
fn open_confined_payload(
    confined: &ConfinedPayload,
    path: &Path,
) -> Result<std::fs::File, PluginError> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use rustix::io::Errno;

    // A symlink or non-directory where confinement found a directory fails the
    // no-follow directory open with ENOTDIR (ELOOP on older Linux kernels); a
    // symlink in place of the payload fails its no-follow open with ELOOP.
    let substituted = |errno: Errno| match errno {
        Errno::LOOP | Errno::NOTDIR => PluginError::InvalidManifest(format!(
            "WASM payload path changed after confinement check: {}",
            path.display()
        )),
        other => PluginError::Io(other.into()),
    };

    let (parent, leaf) = confined.open_payload_parent(|errno, _walked| substituted(errno))?;
    #[cfg(test)]
    payload_step(PayloadStep::BeforeLeafOpen);
    // Non-blocking, so a FIFO or device fails the regular-file check instead of
    // blocking here; `NOCTTY` keeps a terminal from becoming the controlling one.
    let payload = rustix::fs::openat(
        &parent,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(substituted)?;
    #[cfg(test)]
    payload_step(PayloadStep::AfterLeafOpen);

    // The payload came from the directory held since confinement, whatever the
    // root pathname names now. A package whose root was moved or replaced
    // meanwhile is refused rather than admitted under a name that now means
    // another tree.
    let held = rustix::fs::fstat(&confined.root_dir).map_err(std::io::Error::from)?;
    let named = rustix::fs::statat(
        rustix::fs::CWD,
        confined.root.as_path(),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)?;
    if (held.st_dev, held.st_ino) != (named.st_dev, named.st_ino) {
        return Err(PluginError::InvalidManifest(format!(
            "plugin package root changed after confinement check: {}",
            confined.root.display()
        )));
    }
    Ok(std::fs::File::from(payload))
}

/// Open the payload by its absolute pathname, then compare what was opened with
/// the confined root and with what the pathname names.
#[cfg(not(unix))]
fn open_confined_payload(
    confined: &ConfinedPayload,
    path: &Path,
) -> Result<std::fs::File, PluginError> {
    let swapped = || {
        PluginError::InvalidManifest(format!(
            "WASM payload path changed after confinement check: {}",
            path.display()
        ))
    };

    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload is not a regular file: {}",
            path.display()
        )));
    }

    let file = std::fs::File::open(path)?;
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    if same_file::Handle::from_path(&confined.root)? != confined.root_handle {
        return Err(PluginError::InvalidManifest(format!(
            "plugin package root changed after confinement check: {}",
            confined.root.display()
        )));
    }

    let mut prefix = confined.root.clone();
    for segment in &confined.relative {
        prefix.push(segment);
        if std::fs::symlink_metadata(&prefix)?.file_type().is_symlink() {
            return Err(swapped());
        }
    }

    if opened != same_file::Handle::from_path(path)? {
        return Err(swapped());
    }
    Ok(file)
}

/// A boundary between two filesystem operations of payload admission, where a
/// test can rearrange the package tree.
#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PayloadStep {
    /// Confinement opened the package root.
    AfterRootOpen,
    /// A walk opened the directory this many components below the root.
    AfterDir(usize),
    /// The read is about to open the payload.
    BeforeLeafOpen,
    /// The read opened the payload.
    AfterLeafOpen,
}

/// What a test runs at each [`PayloadStep`].
#[cfg(all(test, unix))]
type PayloadStepHook = Box<dyn FnMut(PayloadStep)>;

#[cfg(all(test, unix))]
thread_local! {
    /// Called at every [`PayloadStep`] reached on this thread, when set.
    static PAYLOAD_STEP_HOOK: std::cell::RefCell<Option<PayloadStepHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
fn payload_step(step: PayloadStep) {
    PAYLOAD_STEP_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(step);
        }
    });
}

/// Validate manifest shape: `wasm_path` is required unless the plugin's only
/// capability is `Skill`, and `Skill` plugins must include a `skills/` directory
/// where every subdirectory holds a `SKILL.md` with the agentskills.io required
/// frontmatter fields (`name`, `description`).
/// Reject a manifest-supplied relative path (e.g. `wasm_path`) that could escape the plugin
/// directory. It must be relative and contain no `..`, root, or drive-prefix components — otherwise
/// `plugin_dir.join(p)` / `dest_dir.join(p)` would read or write outside the plugin directory on
/// discovery or install. See GHSA (plugin install wasm_path traversal).
fn validate_manifest_subpath(field: &str, name: &str, p: &str) -> Result<(), PluginError> {
    let path = Path::new(p);
    let escapes = path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        });
    if escapes {
        return Err(PluginError::InvalidManifest(format!(
            "plugin '{name}' has an invalid `{field}` ({p:?}): it must be a relative path inside the plugin directory"
        )));
    }
    Ok(())
}

fn validate_manifest_shape(
    manifest: &PluginManifest,
    plugin_dir: &Path,
) -> Result<(), PluginError> {
    crate::instance::validate_package_name(&manifest.name).map_err(PluginError::InvalidManifest)?;

    if let Some(ref wasm) = manifest.wasm_path {
        validate_manifest_subpath("wasm_path", &manifest.name, wasm)?;
    }

    if manifest.capabilities.is_empty() {
        return Err(PluginError::InvalidManifest(format!(
            "plugin '{}' declares no capabilities",
            manifest.name
        )));
    }

    let is_skill_only =
        manifest.capabilities.len() == 1 && manifest.capabilities[0] == PluginCapability::Skill;

    if !is_skill_only && manifest.wasm_path.is_none() {
        return Err(PluginError::InvalidManifest(format!(
            "plugin '{}' is missing required `wasm_path` for non-skill capabilities",
            manifest.name
        )));
    }

    match (&manifest.wasm_path, &manifest.wasm_sha256) {
        (Some(_), Some(digest)) => signature::validate_sha256_hex(digest)?,
        (None, Some(_)) => {
            return Err(PluginError::InvalidManifest(format!(
                "plugin '{}' declares wasm_sha256 without wasm_path",
                manifest.name
            )));
        }
        _ => {}
    }

    // The `[egress]` declaration is signature-covered content, so a malformed
    // pattern is a malformed package: reject it at discovery and at install
    // rather than silently dropping the entry. This validates the *declaration*
    // only — it still grants nothing, and the grammar it validates
    // against is the same one the operator's grant is validated against.
    zeroclaw_infra::net_guard::normalize_egress_patterns(
        &manifest.egress.hosts,
        &format!("plugin '{}' egress.hosts", manifest.name),
    )
    .map_err(|e| PluginError::InvalidManifest(e.to_string()))?;

    if manifest.capabilities.contains(&PluginCapability::Skill) {
        validate_skill_bundle(&manifest.name, plugin_dir)?;
    }

    Ok(())
}

/// Validate a skill bundle: `<plugin_dir>/skills/` must exist, contain at least
/// one subdirectory, and each subdirectory must hold a `SKILL.md` whose YAML
/// frontmatter declares the agentskills.io-required `name` and `description`.
fn validate_skill_bundle(plugin_name: &str, plugin_dir: &Path) -> Result<(), PluginError> {
    let skills_dir = plugin_dir.join(SKILLS_SUBDIR);
    // Like a package root, the skills root is an admission boundary: a
    // symlink here would let the bundle validated and later copied live
    // outside the package.
    if std::fs::symlink_metadata(&skills_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}' has a symlinked `skills/` directory at {}; package the skills in place",
            plugin_name,
            skills_dir.display()
        )));
    }
    if !skills_dir.is_dir() {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}' is missing `skills/` directory at {}",
            plugin_name,
            skills_dir.display()
        )));
    }

    let mut found_any = false;
    for entry in std::fs::read_dir(&skills_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        found_any = true;
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            return Err(PluginError::InvalidManifest(format!(
                "skill plugin '{}' subdirectory '{}' is missing SKILL.md",
                plugin_name,
                path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            )));
        }
        validate_skill_md_frontmatter(plugin_name, &skill_md)?;
    }

    if !found_any {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}' has empty `skills/` directory",
            plugin_name
        )));
    }

    Ok(())
}

fn validate_skill_md_frontmatter(plugin_name: &str, skill_md: &Path) -> Result<(), PluginError> {
    let content = std::fs::read_to_string(skill_md)?;
    let normalized = content.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n").ok_or_else(|| {
        PluginError::InvalidManifest(format!(
            "skill plugin '{}': {} is missing YAML frontmatter",
            plugin_name,
            skill_md.display()
        ))
    })?;
    let frontmatter = if let Some(idx) = rest.find("\n---\n") {
        &rest[..idx]
    } else if let Some(stripped) = rest.strip_suffix("\n---") {
        stripped
    } else {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}': {} has unterminated frontmatter",
            plugin_name,
            skill_md.display()
        )));
    };

    let mut has_name = false;
    let mut has_description = false;
    for line in frontmatter.lines() {
        let trimmed = line.trim_start();
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            // Treat block-scalar markers as a non-empty value once a continuation
            // line is present; the simple check below is sufficient because the
            // runtime loader parses the actual content.
            let has_value = !value.is_empty();
            match key {
                "name" if has_value => has_name = true,
                "description" if has_value => has_description = true,
                _ => {}
            }
        }
    }

    if !has_name || !has_description {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}': {} frontmatter must declare `name` and `description`",
            plugin_name,
            skill_md.display()
        )));
    }

    Ok(())
}

/// Write an admitted package into `dir`: the exact manifest and component
/// bytes admission read, plus the `skills/` subtree of a skill-capable package.
fn write_package(
    dir: &Path,
    manifest: &PluginManifest,
    manifest_toml: &str,
    source_dir: &Path,
    component: Option<&AdmittedComponent>,
) -> Result<(), PluginError> {
    std::fs::create_dir(dir)?;

    // Persist the exact manifest and payload generations admitted above.
    std::fs::write(dir.join("manifest.toml"), manifest_toml.as_bytes())?;
    if let (Some(rel), Some(component)) = (manifest.wasm_path.as_deref(), component) {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, component.bytes())?;
    }

    // Copy skills/ subtree for skill-capable plugins.
    if manifest.capabilities.contains(&PluginCapability::Skill) {
        let src_skills = source_dir.join(SKILLS_SUBDIR);
        if src_skills.is_dir() {
            copy_dir_recursive(&src_skills, &dir.join(SKILLS_SUBDIR))?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), PluginError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
        // Symlinks intentionally skipped to match the runtime skill auditor.
    }
    Ok(())
}

pub fn migrate_plugins_dir(from: &Path, to: &Path) -> Result<usize, PluginError> {
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(0);
    };

    let mut moved = 0usize;
    for entry in entries.flatten() {
        let src = entry.path();
        if !src.is_dir() || !src.join("manifest.toml").exists() {
            continue;
        }
        let Some(name) = src.file_name() else {
            continue;
        };
        let dest = to.join(name);
        if dest.exists() {
            continue; // never clobber an existing plugin
        }
        std::fs::create_dir_all(to)?;
        // `rename` is atomic but fails across filesystems; fall back to copy+remove.
        if std::fs::rename(&src, &dest).is_err() {
            copy_dir_recursive(&src, &dest)?;
            std::fs::remove_dir_all(&src)?;
        }
        moved += 1;
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loaded set is a hash map; the listing must not inherit its order.
    #[test]
    fn list_plugins_is_sorted_by_name_regardless_of_discovery_order() {
        let dir = tempdir().unwrap();
        for name in ["zeta-plugin", "mid-plugin", "alpha-plugin"] {
            let plugin_dir = dir.path().join("plugins").join(name);
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(
                plugin_dir.join("manifest.toml"),
                format!(
                    r#"
name = "{name}"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = []
"#
                ),
            )
            .unwrap();
            // Discovery admits the declared component, so it has to exist.
            std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
        }

        let host = PluginHost::new(dir.path()).unwrap();
        let names: Vec<String> = host.list_plugins().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["alpha-plugin", "mid-plugin", "zeta-plugin"]);
    }
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
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let plugins = host.list_plugins();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "test-plugin");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_a_symlinked_package_root() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let external = tempdir().unwrap();
        std::fs::write(
            external.path().join("manifest.toml"),
            "name = \"external\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        std::fs::write(external.path().join("plugin.wasm"), b"\0asm").unwrap();

        let plugins_dir = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        symlink(external.path(), plugins_dir.join("linked-package")).unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn discovery_rejects_config_permission_without_a_schema() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("invalid-config");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"invalid-config\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\npermissions = [\"config_read\"]\n",
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn install_rejects_invalid_config_schema_before_copying_files() {
        let source = tempdir().unwrap();
        std::fs::write(
            source.path().join("manifest.toml"),
            "name = \"invalid-config\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\npermissions = [\"config_read\"]\n",
        )
        .unwrap();
        std::fs::write(source.path().join("plugin.wasm"), b"\0asm").unwrap();
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        assert!(host.install(source.path().to_str().unwrap()).is_err());
        assert!(!plugins.path().join("invalid-config").exists());
    }

    #[test]
    fn from_plugins_dir_scans_the_path_directly() {
        // Plugin lives directly under the given dir (no extra `plugins/` level).
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("direct-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
name = "direct-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
        )
        .unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();

        let host = PluginHost::from_plugins_dir(dir.path()).unwrap();
        let plugins = host.list_plugins();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "direct-plugin");
    }

    #[test]
    fn new_still_appends_plugins_subdir() {
        // `new`/`with_security` keep the legacy "workspace dir" contract:
        // a (valid) plugin placed directly under the root is NOT discovered,
        // but the same one under `<root>/plugins/` is.
        let manifest = "name = \"p\"\nversion = \"0.1.0\"\nwasm_path = \"p.wasm\"\ncapabilities = [\"tool\"]\n";

        let dir = tempdir().unwrap();
        let stray = dir.path().join("p");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("manifest.toml"), manifest).unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(
            host.list_plugins().is_empty(),
            "plugin directly under root must not be discovered by `new`"
        );

        // Same manifest under `<root>/plugins/` is found.
        let nested = dir.path().join("plugins").join("p");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("manifest.toml"), manifest).unwrap();
        std::fs::write(nested.join("p.wasm"), b"\0asm").unwrap();
        let host = PluginHost::new(dir.path()).unwrap();
        assert_eq!(host.list_plugins().len(), 1);
        assert_eq!(host.list_plugins()[0].name, "p");
    }

    #[test]
    fn install_then_discover_round_trip_uses_same_dir() {
        // Regression for the install/discovery path divergence
        // a plugin installed into a resolved plugins dir must be discoverable
        // by a fresh host pointed at the *same* dir.
        let src = tempdir().unwrap();
        let manifest = r#"
name = "roundtrip"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        std::fs::write(src.path().join("manifest.toml"), manifest).unwrap();
        std::fs::write(src.path().join("plugin.wasm"), b"\0asm").unwrap();

        let plugins_dir = tempdir().unwrap();
        let mut installer = PluginHost::from_plugins_dir(plugins_dir.path()).unwrap();
        installer
            .install(src.path().to_str().unwrap())
            .expect("install should succeed");
        assert_eq!(
            std::fs::read_to_string(plugins_dir.path().join("roundtrip/manifest.toml")).unwrap(),
            manifest,
            "installation must persist the exact parsed and verified manifest bytes"
        );

        // Fresh host over the same dir — mirrors the CLI install vs. runtime
        // discovery split, both now resolving via `from_plugins_dir`.
        let discoverer = PluginHost::from_plugins_dir(plugins_dir.path()).unwrap();
        let plugins = discoverer.list_plugins();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "roundtrip");
    }

    // Regression (GHSA plugin wasm_path traversal): a `..` wasm_path is rejected on install, so
    // nothing is written outside the plugins directory. Signature mode is Disabled (the default), so
    // the path guard is the only gate.
    #[test]
    fn install_rejects_wasm_path_traversal() {
        let attack_root = tempdir().unwrap();
        let root = attack_root.path();

        // Attacker's package, nested so the traversal's read side resolves inside the package.
        let src_pkg = root.join("a").join("b").join("pkg");
        std::fs::create_dir_all(&src_pkg).unwrap();
        std::fs::write(
            src_pkg.join("manifest.toml"),
            "name = \"evilpkg\"\nversion = \"0.1.0\"\nwasm_path = \"../../pwned.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("a").join("pwned.wasm"),
            b"PWNED-BY-PLUGIN-INSTALL",
        )
        .unwrap();

        let plugins_dir = root.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let mut installer = PluginHost::from_plugins_dir(&plugins_dir).unwrap();

        let res = installer.install(src_pkg.to_str().unwrap());
        assert!(res.is_err(), "install must reject a traversal wasm_path");
        // dest_dir.join("../../pwned.wasm") == root/pwned.wasm — must NOT have been written.
        assert!(
            !root.join("pwned.wasm").exists(),
            "nothing may be written outside the plugins directory"
        );
    }

    // A legitimate relative wasm_path still installs.
    #[test]
    fn install_accepts_legitimate_wasm_path() {
        let src = tempdir().unwrap();
        std::fs::write(
            src.path().join("manifest.toml"),
            "name = \"goodpkg\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        std::fs::write(src.path().join("plugin.wasm"), b"\0asm").unwrap();

        let plugins_dir = tempdir().unwrap();
        let mut installer = PluginHost::from_plugins_dir(plugins_dir.path()).unwrap();
        let name = installer
            .install(src.path().to_str().unwrap())
            .expect("legit install should succeed");
        assert_eq!(name, "goodpkg");
        assert!(
            plugins_dir
                .path()
                .join("goodpkg")
                .join("plugin.wasm")
                .is_file()
        );
    }

    fn write_manifest(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(
            dir.join(name).join("manifest.toml"),
            format!("name = \"{name}\"\nversion = \"0.1.0\"\ncapabilities = [\"tool\"]\n"),
        )
        .unwrap();
    }

    #[test]
    fn migrate_plugins_dir_moves_and_never_clobbers() {
        let from = tempdir().unwrap();
        let to = tempdir().unwrap();
        write_manifest(from.path(), "alpha");
        write_manifest(from.path(), "beta");
        // `beta` already exists in the target → must be skipped, not overwritten.
        write_manifest(to.path(), "beta");

        let moved = migrate_plugins_dir(from.path(), to.path()).unwrap();

        assert_eq!(moved, 1, "only alpha should move; beta collides");
        assert!(to.path().join("alpha").join("manifest.toml").exists());
        assert!(!from.path().join("alpha").exists(), "alpha source removed");
        assert!(
            from.path().join("beta").exists(),
            "skipped source left in place"
        );
    }

    #[test]
    fn migrate_plugins_dir_is_noop_for_missing_or_empty() {
        let to = tempdir().unwrap();
        // Missing source.
        assert_eq!(
            migrate_plugins_dir(&to.path().join("nope"), to.path()).unwrap(),
            0
        );
        // Empty source.
        let empty = tempdir().unwrap();
        assert_eq!(migrate_plugins_dir(empty.path(), to.path()).unwrap(), 0);
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
        std::fs::write(tool_dir.join("tool.wasm"), b"\0asm").unwrap();

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
        std::fs::write(chan_dir.join("channel.wasm"), b"\0asm").unwrap();

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
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();

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
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();

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

    fn write_skill_md(path: &Path, name: &str, description: &str) {
        std::fs::write(
            path,
            format!(
                "---\nname: {name}\ndescription: {description}\n---\n\nBody content for {name}.\n"
            ),
        )
        .unwrap();
    }

    fn write_skill_bundle_plugin(plugins_base: &Path, plugin_name: &str, skill_names: &[&str]) {
        let plugin_dir = plugins_base.join(plugin_name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!("name = \"{plugin_name}\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n"),
        )
        .unwrap();
        let skills_dir = plugin_dir.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        for skill in skill_names {
            let sd = skills_dir.join(skill);
            std::fs::create_dir_all(&sd).unwrap();
            write_skill_md(
                &sd.join("SKILL.md"),
                skill,
                &format!("Description for {skill}"),
            );
        }
    }

    #[test]
    fn test_skill_only_plugin_discovers_without_wasm_path() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");
        write_skill_bundle_plugin(
            &plugins_base,
            "my-toolkit",
            &["design-review", "code-review"],
        );

        let host = PluginHost::new(dir.path()).unwrap();
        let plugins = host.list_plugins();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "my-toolkit");
        assert!(plugins[0].wasm_path.is_none());
        assert!(plugins[0].loaded);

        let skill_plugins = host.skill_plugins();
        assert_eq!(skill_plugins.len(), 1);

        let details = host.skill_plugin_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].0.name, "my-toolkit");
        assert!(details[0].1.ends_with("skills"));
    }

    #[test]
    fn test_non_skill_plugin_without_wasm_path_is_rejected() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("broken");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"broken\"\nversion = \"0.1.0\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        // Discovery skips invalid manifests rather than failing.
        assert!(host.list_plugins().is_empty());
    }

    /// A manifest with an unparseable `[egress]` grammar is rejected outright
    /// rather than having the bad entry dropped: silently discarding a
    /// destination the publisher believed they declared is how a declaration
    /// and the operator's seeded grant drift apart.
    #[test]
    fn invalid_egress_grammar_rejects_the_manifest_at_discovery() {
        for bad in [
            "\"*\"",
            "\"*.com\"",
            "\"https://api.example.com\"",
            "\"api.example.com:8443\"",
        ] {
            let dir = tempdir().unwrap();
            let plugin_dir = dir.path().join("plugins").join("bad-egress");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(
                plugin_dir.join("manifest.toml"),
                format!(
                    "name = \"bad-egress\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n\n[egress]\nhosts = [{bad}]\n"
                ),
            )
            .unwrap();

            let host = PluginHost::new(dir.path()).unwrap();
            assert!(
                host.list_plugins().is_empty(),
                "egress entry {bad} must reject the manifest"
            );
        }
    }

    #[test]
    fn invalid_egress_grammar_rejects_the_manifest_at_install() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(
            source.join("manifest.toml"),
            "name = \"bad-egress\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n\n[egress]\nhosts = [\"*\"]\n",
        )
        .unwrap();

        let mut host = PluginHost::new(&dir.path().join("home")).unwrap();
        let err = host
            .install(source.to_str().unwrap())
            .expect_err("install must refuse an invalid egress declaration");
        assert!(
            format!("{err}").contains("allow-all"),
            "unexpected error: {err}"
        );
    }

    /// A valid declaration parses, survives discovery, and — critically — still
    /// grants nothing. Reach comes only from the operator's config entry.
    #[test]
    fn valid_egress_declaration_is_admitted_but_confers_no_reach() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("declares");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"declares\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\npermissions = [\"http_client\"]\n\n[egress]\nhosts = [\"api.example.com\", \"*.cdn.example.com\"]\n",
        )
        .unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        let plugins = host.list_plugins();
        assert_eq!(plugins.len(), 1);
        let manifest = &host.loaded.get("declares").unwrap().manifest;
        assert_eq!(
            manifest.egress.hosts,
            vec![
                "api.example.com".to_string(),
                "*.cdn.example.com".to_string()
            ]
        );
        // `PluginInfo` is the surface the CLI and registry render. The
        // declaration is deliberately absent from it as a grant-shaped value.
        assert!(
            plugins[0]
                .permissions
                .contains(&crate::PluginPermission::HttpClient)
        );
    }

    /// A manifest written before this field parses unchanged and declares
    /// nothing — absent and empty are the same state.
    #[test]
    fn manifest_without_an_egress_table_declares_nothing() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("legacy");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"legacy\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\npermissions = [\"http_client\"]\n",
        )
        .unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert_eq!(host.list_plugins().len(), 1);
        assert!(
            host.loaded
                .get("legacy")
                .unwrap()
                .manifest
                .egress
                .hosts
                .is_empty()
        );
    }

    #[test]
    fn manifest_name_must_be_a_canonical_package_slug() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("unsafe-name");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"../escape\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn duplicate_package_names_are_all_rejected() {
        let dir = tempdir().unwrap();
        let plugins_dir = dir.path().join("plugins");
        for directory in ["first", "second"] {
            let plugin_dir = plugins_dir.join(directory);
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(
                plugin_dir.join("manifest.toml"),
                "name = \"shared\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
            )
            .unwrap();
        }

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.get_plugin("shared").is_none());
    }

    #[test]
    fn test_skill_plugin_missing_skills_dir_is_rejected() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("empty-skills");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"empty-skills\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n",
        )
        .unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn test_skill_plugin_rejects_skill_without_required_frontmatter() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("bad-frontmatter");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"bad-frontmatter\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n",
        )
        .unwrap();
        let skill_dir = plugin_dir.join("skills").join("oops");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // Missing description field
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: oops\n---\n\nbody\n").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn test_skill_plugin_rejects_skill_without_skill_md() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("missing-md");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            "name = \"missing-md\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n",
        )
        .unwrap();
        let skill_dir = plugin_dir.join("skills").join("orphan");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("notes.md"), "no SKILL.md here").unwrap();

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn test_skill_plugin_does_not_appear_in_tool_or_channel_lists() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");
        write_skill_bundle_plugin(&plugins_base, "skill-bundle", &["one"]);

        let host = PluginHost::new(dir.path()).unwrap();
        assert!(host.tool_plugins().is_empty());
        assert!(host.tool_plugin_details().is_empty());
        assert!(host.channel_plugins().is_empty());
        assert_eq!(host.skill_plugins().len(), 1);
    }

    fn write_unsigned_tool_plugin(plugins_dir: &Path, name: &str) {
        let plugin_dir = plugins_dir.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\ncapabilities = [\"tool\"]\nwasm_path = \"plugin.wasm\"\n"
            ),
        )
        .unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
    }

    fn write_channel_plugin(plugins_dir: &Path, name: &str, with_wasm: bool) {
        let plugin_dir = plugins_dir.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let wasm_line = if with_wasm {
            "wasm_path = \"plugin.wasm\"\n"
        } else {
            ""
        };
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\ncapabilities = [\"channel\"]\n{wasm_line}"
            ),
        )
        .unwrap();
        if with_wasm {
            std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
        }
    }

    #[test]
    fn channel_plugin_details_yields_only_wasm_backed_channels() {
        let dir = tempdir().unwrap();
        let plugins_base = dir.path().join("plugins");
        write_channel_plugin(&plugins_base, "with-wasm", true);
        write_channel_plugin(&plugins_base, "no-wasm", false);

        let host = PluginHost::new(dir.path()).unwrap();
        let details = host.channel_plugin_details();
        assert_eq!(
            details.len(),
            1,
            "a channel manifest with no wasm_path is not registrable as a live channel"
        );
        assert_eq!(details[0].0.name, "with-wasm");
        assert_eq!(details[0].1.bytes(), b"\0asm");
    }

    #[test]
    fn from_plugins_dir_with_security_strict_drops_unsigned_plugin() {
        let dir = tempdir().unwrap();
        write_unsigned_tool_plugin(dir.path(), "unsigned-tool");

        let host = PluginHost::from_plugins_dir_with_security(
            dir.path(),
            SignatureMode::Strict,
            Vec::new(),
        )
        .unwrap();

        assert!(
            host.list_plugins().is_empty(),
            "strict mode must reject an unsigned plugin during discovery"
        );
    }

    #[test]
    fn admit_source_rejects_unsigned_plugin_before_load_verification() {
        let source = tempdir().unwrap();
        std::fs::write(
            source.path().join("manifest.toml"),
            "name = \"unsigned-source\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        std::fs::write(source.path().join("plugin.wasm"), b"not a component").unwrap();

        let plugins = tempdir().unwrap();
        let host = PluginHost::from_plugins_dir_with_security(
            plugins.path(),
            SignatureMode::Strict,
            Vec::new(),
        )
        .unwrap();

        let err = host
            .admit_source(source.path().to_str().unwrap())
            .expect_err("strict policy must reject an unsigned source before load verification");
        assert!(matches!(err, PluginError::UnsignedPlugin(_)));
    }

    /// A signature is checked against manifest text, so the text an admitted
    /// source and a loaded plugin report is the text admission read, byte for
    /// byte, comments and spacing included.
    #[test]
    fn admitted_and_loaded_manifest_text_is_the_text_admission_read() {
        let text = "# Published by the test suite.\nname = \"text-plugin\"\nversion   = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n";
        let source = tempdir().unwrap();
        std::fs::write(source.path().join("manifest.toml"), text).unwrap();
        std::fs::write(source.path().join("plugin.wasm"), b"\0asm").unwrap();
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        let admitted = host.admit_source(source.path().to_str().unwrap()).unwrap();
        assert_eq!(admitted.manifest_toml(), text);
        host.install_admitted(admitted).unwrap();
        assert_eq!(host.manifest_toml("text-plugin"), Some(text));

        let rediscovered = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(
            rediscovered.manifest_toml("text-plugin"),
            Some(text),
            "discovery keeps the text it read from the plugins directory"
        );
        assert_eq!(rediscovered.manifest_toml("missing-plugin"), None);
    }

    #[test]
    fn admit_source_rejects_invalid_config_before_load_verification() {
        let source = tempdir().unwrap();
        std::fs::write(
            source.path().join("manifest.toml"),
            "name = \"invalid-config-source\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\npermissions = [\"config_read\"]\n",
        )
        .unwrap();
        std::fs::write(source.path().join("plugin.wasm"), b"not a component").unwrap();

        let plugins = tempdir().unwrap();
        let host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        let err = host
            .admit_source(source.path().to_str().unwrap())
            .expect_err("invalid config must be rejected before load verification");
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    fn write_tool_source(dir: &Path, name: &str, wasm: &[u8]) {
        std::fs::write(
            dir.join("manifest.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.wasm"), wasm).unwrap();
    }

    /// A second source carrying an already-loaded name is refused before its
    /// component is read. The proof is observable: the candidate's component
    /// is a sparse file past the admission size limit, so reading it would
    /// have failed with the size-limit error, yet the answer is
    /// `AlreadyLoaded`, and the installed component still holds the first
    /// source's bytes.
    #[test]
    fn admit_source_refuses_a_duplicate_name_before_touching_its_component() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        let first = tempdir().unwrap();
        write_tool_source(first.path(), "dup", b"\0asm first");
        host.install(first.path().to_str().unwrap()).unwrap();

        let second = tempdir().unwrap();
        write_tool_source(second.path(), "dup", b"");
        std::fs::File::options()
            .write(true)
            .open(second.path().join("plugin.wasm"))
            .unwrap()
            .set_len(MAX_COMPONENT_BYTES + 1)
            .unwrap();
        let err = host
            .admit_source(second.path().to_str().unwrap())
            .expect_err("a loaded name must be refused at admission");
        assert!(
            matches!(err, PluginError::AlreadyLoaded(ref name) if name == "dup"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(plugins.path().join("dup/plugin.wasm")).unwrap(),
            b"\0asm first",
            "the installed component is untouched"
        );
    }

    /// The local install path is bounded like every other admission: a
    /// component whose length exceeds the limit is refused from its metadata,
    /// before a byte is read or buffered. A sparse file makes the case cheap
    /// and deterministic, since its logical length costs no disk.
    #[test]
    fn admit_source_refuses_an_oversized_component_before_reading_it() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let source = tempdir().unwrap();
        write_tool_source(source.path(), "huge", b"");
        std::fs::File::options()
            .write(true)
            .open(source.path().join("plugin.wasm"))
            .unwrap()
            .set_len(MAX_COMPONENT_BYTES + 1)
            .unwrap();

        let err = host
            .admit_source(source.path().to_str().unwrap())
            .expect_err("an oversized local component must be refused");
        assert!(err.to_string().contains("admission limit"), "{err}");
        let err = host
            .install(source.path().to_str().unwrap())
            .expect_err("install goes through the same admission");
        assert!(err.to_string().contains("admission limit"), "{err}");
        assert!(host.get_plugin("huge").is_none(), "nothing was installed");
        assert!(!plugins.path().join("huge").exists());
    }

    /// What was admitted is what gets installed: a source swapped after
    /// admission (the window in which the CLI runs its load-check) does not
    /// change the installed bytes.
    #[test]
    fn install_persists_the_admitted_bytes_not_the_source_after_a_swap() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let source = tempdir().unwrap();
        write_tool_source(source.path(), "swapped", b"\0asm admitted");

        let admitted = host.admit_source(source.path().to_str().unwrap()).unwrap();
        let component = admitted.component().expect("a tool ships a component");
        assert_eq!(component.bytes(), b"\0asm admitted");

        // The source changes underneath, as an attacker racing the install
        // would do.
        std::fs::write(source.path().join("plugin.wasm"), b"\0asm swapped in").unwrap();

        let name = host.install_admitted(admitted).unwrap();
        assert_eq!(name, "swapped");
        assert_eq!(
            std::fs::read(plugins.path().join("swapped/plugin.wasm")).unwrap(),
            b"\0asm admitted",
            "the verified bytes are the installed bytes"
        );
    }

    /// An install that fails part-way leaves nothing under the package name and
    /// no staging directory, so the retry after the cause is fixed succeeds
    /// instead of hitting `AlreadyLoaded` on a half-written package. The
    /// failure is real: admission reads only each skill's `SKILL.md`, so an
    /// unreadable extra file passes admission and fails the copy.
    #[cfg(unix)]
    #[test]
    fn a_failed_install_leaves_nothing_behind_and_the_retry_succeeds() {
        use std::os::unix::fs::PermissionsExt;

        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let source = tempdir().unwrap();
        write_skill_bundle_plugin(source.path(), "half", &["alpha"]);
        let source_dir = source.path().join("half");
        let unreadable = source_dir.join("skills/alpha/notes.txt");
        std::fs::write(&unreadable, "extra").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&unreadable).is_ok() {
            // Running as root: permissions cannot make the copy fail.
            return;
        }

        let admitted = host.admit_source(source_dir.to_str().unwrap()).unwrap();
        assert!(host.install_admitted(admitted).is_err());
        let left: Vec<_> = std::fs::read_dir(plugins.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "a failed install left {left:?} behind");
        assert!(host.get_plugin("half").is_none());

        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();
        let admitted = host.admit_source(source_dir.to_str().unwrap()).unwrap();
        assert_eq!(host.install_admitted(admitted).unwrap(), "half");
        assert!(plugins.path().join("half/skills/alpha/notes.txt").is_file());
    }

    /// A staging directory stranded by a crash is never discovered as a
    /// package, even though it holds a complete manifest.
    #[test]
    fn discovery_skips_a_stranded_staging_directory() {
        let plugins = tempdir().unwrap();
        let staging = plugins.path().join(".stranded.installing-4242");
        std::fs::create_dir_all(&staging).unwrap();
        write_tool_source(&staging, "stranded", b"\0asm");

        let host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    /// A symlinked `skills/` root is refused at admission, like a symlinked
    /// package root: the bundle validated and then copied must live inside
    /// the package.
    #[cfg(unix)]
    #[test]
    fn admit_source_refuses_a_symlinked_skills_root() {
        use std::os::unix::fs::symlink;

        let plugins = tempdir().unwrap();
        let host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let source = tempdir().unwrap();
        let external = tempdir().unwrap();
        write_skill_bundle_plugin(external.path(), "ext", &["alpha"]);
        write_skill_bundle_plugin(source.path(), "linked", &["alpha"]);
        let source_dir = source.path().join("linked");
        std::fs::remove_dir_all(source_dir.join("skills")).unwrap();
        symlink(
            external.path().join("ext/skills"),
            source_dir.join("skills"),
        )
        .unwrap();

        let err = host
            .admit_source(source_dir.to_str().unwrap())
            .expect_err("a symlinked skills root must be refused");
        assert!(
            err.to_string().contains("symlinked"),
            "unexpected error: {err}"
        );
    }

    /// A `wasm_path` that is a symlink is refused: the component must be a
    /// regular file inside the source, so the bytes admitted are the bytes at
    /// that path and not whatever the link points at by the time of the read.
    #[cfg(unix)]
    #[test]
    fn admit_source_refuses_a_symlinked_component() {
        let plugins = tempdir().unwrap();
        let host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let elsewhere = tempdir().unwrap();
        std::fs::write(elsewhere.path().join("real.wasm"), b"\0asm elsewhere").unwrap();
        let source = tempdir().unwrap();
        std::fs::write(
            source.path().join("manifest.toml"),
            "name = \"linked\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            elsewhere.path().join("real.wasm"),
            source.path().join("plugin.wasm"),
        )
        .unwrap();

        let err = host
            .admit_source(source.path().to_str().unwrap())
            .expect_err("a symlinked component must be refused");
        assert!(matches!(err, PluginError::InvalidManifest(_)), "{err}");
    }

    #[test]
    fn strict_discovery_verifies_the_config_schema_as_signed_content() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("signed-schema");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
        let unsigned = format!(
            r#"name = "signed-schema"
version = "0.1.0"
wasm_path = "plugin.wasm"
wasm_sha256 = "{}"
capabilities = ["tool"]
permissions = ["config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
required = ["retries"]
additionalProperties = false

[config_schema.properties.retries]
type = "integer"
minimum = 1
"#,
            signature::sha256_hex(b"\0asm")
        );
        let (private_key, publisher_key) = signature::generate_signing_key().unwrap();
        let signed_value = signature::sign_manifest(&unsigned, &private_key).unwrap();
        let signed = unsigned.replacen(
            "wasm_path = \"plugin.wasm\"",
            &format!(
                "signature = \"{signed_value}\"\npublisher_key = \"{publisher_key}\"\nwasm_path = \"plugin.wasm\""
            ),
            1,
        );
        std::fs::write(plugin_dir.join("manifest.toml"), &signed).unwrap();

        let host = PluginHost::from_plugins_dir_with_security(
            dir.path(),
            SignatureMode::Strict,
            vec![publisher_key.clone()],
        )
        .unwrap();
        assert_eq!(host.list_plugins().len(), 1);

        let tampered = signed.replace("minimum = 1", "minimum = 2");
        std::fs::write(plugin_dir.join("manifest.toml"), tampered).unwrap();
        let host = PluginHost::from_plugins_dir_with_security(
            dir.path(),
            SignatureMode::Strict,
            vec![publisher_key],
        )
        .unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn strict_discovery_requires_a_digest_after_signature_verifies() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("signed-without-digest");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"component bytes").unwrap();
        let unsigned = r#"name = "signed-without-digest"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let (private_key, publisher_key) = signature::generate_signing_key().unwrap();
        let signed_value = signature::sign_manifest(unsigned, &private_key).unwrap();
        let signed = unsigned.replacen(
            "wasm_path = \"plugin.wasm\"",
            &format!(
                "signature = \"{signed_value}\"\npublisher_key = \"{publisher_key}\"\nwasm_path = \"plugin.wasm\""
            ),
            1,
        );
        std::fs::write(plugin_dir.join("manifest.toml"), signed).unwrap();

        let host = PluginHost::from_plugins_dir_with_security(
            dir.path(),
            SignatureMode::Strict,
            vec![publisher_key],
        )
        .unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[test]
    fn from_plugins_dir_with_security_disabled_loads_unsigned_plugin() {
        let dir = tempdir().unwrap();
        write_unsigned_tool_plugin(dir.path(), "unsigned-tool");

        let host = PluginHost::from_plugins_dir_with_security(
            dir.path(),
            SignatureMode::Disabled,
            Vec::new(),
        )
        .unwrap();

        assert_eq!(
            host.list_plugins().len(),
            1,
            "disabled mode must load an unsigned plugin"
        );
    }

    #[test]
    fn from_plugins_dir_with_security_permissive_loads_unsigned_plugin() {
        let dir = tempdir().unwrap();
        write_unsigned_tool_plugin(dir.path(), "unsigned-tool");

        let host = PluginHost::from_plugins_dir_with_security(
            dir.path(),
            SignatureMode::Permissive,
            Vec::new(),
        )
        .unwrap();

        assert_eq!(
            host.list_plugins().len(),
            1,
            "permissive mode must load an unsigned plugin (untrusted and invalid signatures also load with a warning in permissive mode, covered in signature.rs)"
        );
    }

    #[test]
    fn admitted_component_retains_the_exact_verified_bytes() {
        let root = tempdir().unwrap();
        let plugin_dir = root.path().join("exact-bytes");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let admitted_bytes = b"first component generation";
        std::fs::write(plugin_dir.join("plugin.wasm"), admitted_bytes).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "name = \"exact-bytes\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\nwasm_sha256 = \"{}\"\ncapabilities = [\"tool\"]\n",
                signature::sha256_hex(admitted_bytes)
            ),
        )
        .unwrap();

        let host = PluginHost::from_plugins_dir(root.path()).unwrap();
        let component = host.tool_plugin_details()[0].1.clone();
        std::fs::write(
            plugin_dir.join("plugin.wasm"),
            b"second component generation",
        )
        .unwrap();

        assert_eq!(component.bytes(), admitted_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn stable_payload_read_rejects_a_post_confinement_symlink_swap() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::create_dir_all(&package).unwrap();
        let payload = package.join("plugin.wasm");
        std::fs::write(&payload, b"inside").unwrap();
        let confined = resolve_confined_wasm_path(&package, "plugin.wasm").unwrap();
        let outside = root.path().join("outside.wasm");
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::remove_file(&payload).unwrap();
        symlink(&outside, &payload).unwrap();

        assert!(matches!(
            read_stable_file(&confined),
            Err(PluginError::InvalidManifest(_))
        ));
    }

    #[test]
    fn stable_payload_read_rejects_a_payload_over_the_admission_limit() {
        let root = tempdir().unwrap();
        let package = root.path().join("plugins").join("pkg");
        std::fs::create_dir_all(&package).unwrap();

        // Sparse: reports an oversized length without writing the bytes, so the
        // stat-side rejection is exercised without allocating 64 MiB in a test.
        let payload = package.join("plugin.wasm");
        let file = std::fs::File::create(&payload).unwrap();
        file.set_len(MAX_COMPONENT_BYTES + 1).unwrap();
        drop(file);

        let confined = resolve_confined_wasm_path(&package, "plugin.wasm").unwrap();
        let read = read_stable_file(&confined);

        let Err(PluginError::InvalidManifest(message)) = read else {
            panic!("oversized payload was admitted: {read:?}");
        };
        assert!(
            message.contains("admission limit"),
            "rejection should name the limit, got: {message}"
        );
    }

    #[test]
    fn stable_payload_read_admits_a_payload_at_the_admission_limit() {
        let root = tempdir().unwrap();
        let package = root.path().join("plugins").join("pkg");
        std::fs::create_dir_all(&package).unwrap();

        // The boundary itself is allowed: the limit rejects what exceeds it, not
        // what reaches it. Kept small-but-real so the read path is exercised.
        let payload = package.join("plugin.wasm");
        let contents = vec![7u8; 4096];
        std::fs::write(&payload, &contents).unwrap();

        let confined = resolve_confined_wasm_path(&package, "plugin.wasm").unwrap();
        assert_eq!(read_stable_file(&confined).unwrap(), contents);
    }

    #[cfg(unix)]
    #[test]
    fn stable_payload_read_rejects_a_post_confinement_package_root_swap() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let package = root.path().join("plugins").join("pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("plugin.wasm"), b"admitted component").unwrap();
        let confined = resolve_confined_wasm_path(&package, "plugin.wasm").unwrap();

        let attacker = root.path().join("attacker");
        std::fs::create_dir_all(&attacker).unwrap();
        std::fs::write(attacker.join("plugin.wasm"), b"attacker component").unwrap();

        std::fs::rename(&package, root.path().join("plugins").join("pkg-moved")).unwrap();
        symlink(&attacker, &package).unwrap();

        let read = read_stable_file(&confined);
        assert!(
            !matches!(&read, Ok(bytes) if bytes.as_slice() == b"attacker component"),
            "package-root swap admitted attacker bytes"
        );
        assert!(matches!(read, Err(PluginError::InvalidManifest(_))));
    }

    #[cfg(unix)]
    #[test]
    fn stable_payload_read_rejects_a_post_confinement_intermediate_directory_swap() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let package = root.path().join("pkg");
        let nested = package.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("plugin.wasm"), b"admitted component").unwrap();
        let confined = resolve_confined_wasm_path(&package, "nested/plugin.wasm").unwrap();

        let attacker = root.path().join("attacker");
        std::fs::create_dir_all(&attacker).unwrap();
        std::fs::write(attacker.join("plugin.wasm"), b"attacker component").unwrap();

        std::fs::rename(&nested, package.join("nested-moved")).unwrap();
        symlink(&attacker, &nested).unwrap();

        let read = read_stable_file(&confined);
        assert!(
            !matches!(&read, Ok(bytes) if bytes.as_slice() == b"attacker component"),
            "intermediate-directory swap admitted attacker bytes"
        );
        assert!(matches!(read, Err(PluginError::InvalidManifest(_))));
    }

    /// Move the directory at `path` to `aside` and put a symlink to
    /// `replacement` in its place.
    #[cfg(unix)]
    fn swap_in(path: &Path, aside: &Path, replacement: &Path) {
        std::fs::rename(path, aside).unwrap();
        std::os::unix::fs::symlink(replacement, path).unwrap();
    }

    /// Undo `swap_in`.
    #[cfg(unix)]
    fn swap_back(path: &Path, aside: &Path) {
        std::fs::remove_file(path).unwrap();
        std::fs::rename(aside, path).unwrap();
    }

    /// Run `admit` with `hook` called at every payload admission step reached
    /// on this thread.
    #[cfg(unix)]
    fn with_payload_steps<T>(
        hook: impl FnMut(PayloadStep) + 'static,
        admit: impl FnOnce() -> T,
    ) -> T {
        PAYLOAD_STEP_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        let admitted = admit();
        PAYLOAD_STEP_HOOK.with(|slot| *slot.borrow_mut() = None);
        admitted
    }

    /// The package root is replaced by a symlink to an attacker tree during
    /// admission. Confinement opened the root before the swap and every later
    /// open descends from that handle, so admission refuses the moved root or
    /// reads the original payload; it never reads the attacker's. A payload in
    /// the root and one below a directory are both covered, so resolving the
    /// root, a directory, or the payload by pathname during the read fails here.
    #[cfg(unix)]
    #[test]
    fn payload_read_survives_root_replacement_between_steps() {
        for relative in ["plugin.wasm", "nested/plugin.wasm"] {
            let root = tempdir().unwrap();
            let package = root.path().join("plugins").join("pkg");
            let aside = root.path().join("plugins").join("pkg-moved");
            let attacker = root.path().join("attacker");
            for (tree, contents) in [
                (&package, "admitted component"),
                (&attacker, "attacker component"),
            ] {
                let payload = tree.join(relative);
                std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
                std::fs::write(payload, contents).unwrap();
            }

            // Replaced right after confinement opened the root, and left that
            // way: the walks still reach the original payload, and the read
            // refuses a root the pathname no longer names.
            let mut swap = Some((package.clone(), aside.clone(), attacker.clone()));
            let read = with_payload_steps(
                move |step| {
                    if step == PayloadStep::AfterRootOpen
                        && let Some((package, aside, attacker)) = swap.take()
                    {
                        swap_in(&package, &aside, &attacker);
                    }
                },
                || {
                    resolve_confined_wasm_path(&package, relative)
                        .and_then(|confined| read_stable_file(&confined))
                },
            );
            assert!(
                matches!(read, Err(PluginError::InvalidManifest(_))),
                "{relative}: a root replaced mid-admission was admitted: {read:?}"
            );
            swap_back(&package, &aside);

            // Replaced after confinement and before the read starts, and
            // restored once the payload is open, so every check after the open
            // sees the original root. Resolving the root, a directory, or the
            // payload by pathname during the read would reach the attacker
            // tree; the walk from the retained root reads the admitted file.
            let confined = resolve_confined_wasm_path(&package, relative).unwrap();
            swap_in(&package, &aside, &attacker);
            let read = with_payload_steps(
                move |step| {
                    if step == PayloadStep::AfterLeafOpen {
                        swap_back(&package, &aside);
                    }
                },
                || read_stable_file(&confined),
            );
            assert_eq!(
                String::from_utf8_lossy(&read.unwrap()),
                "admitted component",
                "{relative}"
            );
        }
    }

    /// A directory between the root and the payload is replaced by a symlink
    /// to an attacker tree during admission. The walks open each directory
    /// without following a symlink, so a symlink the walk reaches is refused,
    /// and a replacement made after the walk passed is never consulted: the
    /// payload is opened from the directory the walk already holds.
    #[cfg(unix)]
    #[test]
    fn payload_read_survives_intermediate_replacement_between_steps() {
        let root = tempdir().unwrap();
        let package = root.path().join("pkg");
        let nested = package.join("nested");
        let aside = package.join("nested-moved");
        let attacker = root.path().join("attacker");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&attacker).unwrap();
        std::fs::write(nested.join("plugin.wasm"), b"admitted component").unwrap();
        std::fs::write(attacker.join("plugin.wasm"), b"attacker component").unwrap();

        // Replaced once confinement's walk has opened `nested`, and left that
        // way: confinement finishes from the directory it holds, and the read's
        // walk refuses the symlink it then finds at `nested`.
        let mut swap = Some((nested.clone(), aside.clone(), attacker.clone()));
        let read = with_payload_steps(
            move |step| {
                if step == PayloadStep::AfterDir(1)
                    && let Some((nested, aside, attacker)) = swap.take()
                {
                    swap_in(&nested, &aside, &attacker);
                }
            },
            || {
                resolve_confined_wasm_path(&package, "nested/plugin.wasm")
                    .and_then(|confined| read_stable_file(&confined))
            },
        );
        assert!(
            matches!(read, Err(PluginError::InvalidManifest(_))),
            "a directory replaced mid-admission was followed: {read:?}"
        );
        swap_back(&nested, &aside);

        // Replaced after the read's walk opened `nested`, and restored once the
        // payload is open: the open is relative to the directory the walk
        // holds, so it reads the admitted file while the pathname names the
        // attacker's.
        let confined = resolve_confined_wasm_path(&package, "nested/plugin.wasm").unwrap();
        let read = with_payload_steps(
            move |step| match step {
                PayloadStep::AfterDir(1) => swap_in(&nested, &aside, &attacker),
                PayloadStep::AfterLeafOpen => swap_back(&nested, &aside),
                _ => {}
            },
            || read_stable_file(&confined),
        );
        assert_eq!(
            String::from_utf8_lossy(&read.unwrap()),
            "admitted component"
        );
    }

    /// A FIFO swapped in for the payload after every check that precedes the
    /// open fails the read at once instead of blocking admission on a writer
    /// that never comes: the payload is opened non-blocking and must be a
    /// regular file. Confinement refuses a FIFO that is already in place.
    #[cfg(unix)]
    #[test]
    fn payload_read_fails_fast_on_a_fifo() {
        let root = tempdir().unwrap();
        let package = root.path().join("pkg");
        std::fs::create_dir_all(&package).unwrap();
        let payload = package.join("plugin.wasm");
        std::fs::write(&payload, b"admitted component").unwrap();
        let confined = resolve_confined_wasm_path(&package, "plugin.wasm").unwrap();

        let fifo = payload.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let read = with_payload_steps(
                move |step| {
                    if step == PayloadStep::BeforeLeafOpen {
                        std::fs::remove_file(&fifo).unwrap();
                        assert!(
                            std::process::Command::new("mkfifo")
                                .arg(&fifo)
                                .status()
                                .unwrap()
                                .success()
                        );
                    }
                },
                || read_stable_file(&confined),
            );
            sender.send(read).unwrap();
        });
        let read = match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(read) => read,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("a FIFO in place of the payload blocked the read")
            }
            // The reader panicked before sending (in the hook, say): report
            // that failure rather than a blocked read.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::panic::resume_unwind(
                reader
                    .join()
                    .expect_err("the reader sends before it returns"),
            ),
        };
        assert!(
            matches!(&read, Err(PluginError::InvalidManifest(message)) if message.contains("not a regular file")),
            "{read:?}"
        );

        assert!(matches!(
            resolve_confined_wasm_path(&package, "plugin.wasm"),
            Err(PluginError::InvalidManifest(_))
        ));
    }

    #[test]
    fn parse_signature_mode_maps_config_strings() {
        assert_eq!(
            PluginHost::parse_signature_mode("strict"),
            Some(SignatureMode::Strict)
        );
        assert_eq!(
            PluginHost::parse_signature_mode("permissive"),
            Some(SignatureMode::Permissive)
        );
        assert_eq!(
            PluginHost::parse_signature_mode("disabled"),
            Some(SignatureMode::Disabled)
        );
        // Case-insensitive: to_lowercase normalizes before matching.
        assert_eq!(
            PluginHost::parse_signature_mode("STRICT"),
            Some(SignatureMode::Strict)
        );
        // Unrecognized values return None so the caller fails safe instead of
        // silently degrading to the weakest posture on a config typo.
        assert_eq!(PluginHost::parse_signature_mode("nonsense"), None);
        assert_eq!(PluginHost::parse_signature_mode("sttict"), None);
    }
}
