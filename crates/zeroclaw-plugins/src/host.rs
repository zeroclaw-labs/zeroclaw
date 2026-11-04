//! Plugin host: discovery, loading, lifecycle management.

mod recovery;
#[cfg(test)]
mod recovery_tests;

use super::error::PluginError;
use super::signature::{self, SignatureMode};
use super::{PluginCapability, PluginInfo, PluginManifest};
use crate::config::validate_manifest_config;
use cap_std::fs::Dir;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Subdirectory inside a skill-capable plugin that holds individual skills.
const SKILLS_SUBDIR: &str = "skills";

/// Manages the lifecycle of WASM plugins.
pub struct PluginHost {
    plugins_dir: PathBuf,
    /// Opened on first use; see [`opened_root`].
    recovery_root: std::sync::OnceLock<recovery::Root>,
    loaded: HashMap<String, LoadedPlugin>,
    signature_mode: SignatureMode,
    trusted_publisher_keys: Vec<String>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    /// The manifest text `manifest` was parsed from and its signature was
    /// checked against: the exact text admitted with this generation, so an
    /// update can tell whether the package it claims is still the one loaded
    /// here.
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

/// What a source is admitted as.
#[derive(Clone, Copy)]
enum AdmissionTarget<'a> {
    /// A new package, whose name must not be loaded.
    NewPackage,
    /// The replacement for the loaded package of this name.
    Replacement(&'a str),
}

/// What [`PluginHost::update_admitted`] replaced.
#[derive(Debug)]
pub struct ReplacedPackage {
    /// The version of the package that was replaced.
    pub previous_version: String,
    /// Set when the replaced generation could not be deleted afterwards:
    /// where it was left, and why. It stays in its hidden transaction, which
    /// discovery ignores, marked for deletion, so recovery only ever deletes
    /// it. If even that marking failed, it is still a claimed generation, which
    /// recovery deletes while the package is installed but would put back once
    /// the package is removed, so it is best deleted by hand.
    pub leftover: Option<(PathBuf, String)>,
}

/// What [`PluginHost::recover_interrupted_update`] found for one package.
///
/// Every outcome but `Nothing` carries `kept`: the generations marked for
/// deletion that recovery found and could not delete, each with where it was
/// left and why. [`UpdateRecovery::kept`] returns them whatever else recovery
/// did.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateRecovery {
    /// There was nothing to put back or delete. Abandoned stages and empty
    /// transactions may have been cleared.
    Nothing,
    /// Out-of-date generations earlier updates claimed, or marked for
    /// deletion, were found. `removed` were deleted. `kept` could not be, each
    /// with where it was left and why: a generation marked for deletion is
    /// never loaded or put back, while one whose marking failed is still a
    /// claim.
    Swept {
        removed: usize,
        kept: Vec<(PathBuf, String)>,
    },
    /// The package was missing, and the one generation an interrupted update
    /// claimed was put back and discovery ran again. The configured policy
    /// decides whether it is admitted; [`PluginHost::get_plugin`] says whether
    /// it was.
    Restored { kept: Vec<(PathBuf, String)> },
    /// The package is missing and more than one interrupted update claimed a
    /// generation of it, so which was installed last is not known. Nothing
    /// was moved.
    Ambiguous {
        displaced: Vec<PathBuf>,
        kept: Vec<(PathBuf, String)>,
    },
    /// The package is not loaded, but something already occupies its name, so
    /// the claimed generation was not moved onto it.
    Occupied {
        displaced: PathBuf,
        occupant: PathBuf,
        kept: Vec<(PathBuf, String)>,
    },
    /// [`PluginHost::sweep_interrupted_update`] found the package missing and
    /// a generation of it that an interrupted update claimed, and left the
    /// claim where it is: only recovering the package by name puts it back.
    Displaced {
        displaced: Vec<PathBuf>,
        kept: Vec<(PathBuf, String)>,
    },
}

impl UpdateRecovery {
    /// The generations marked for deletion that recovery found and could not
    /// delete, whatever else it did: each with where it was left and why.
    #[must_use]
    pub fn kept(&self) -> &[(PathBuf, String)] {
        match self {
            Self::Nothing => &[],
            Self::Swept { kept, .. }
            | Self::Restored { kept }
            | Self::Ambiguous { kept, .. }
            | Self::Occupied { kept, .. }
            | Self::Displaced { kept, .. } => kept,
        }
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
            recovery_root: std::sync::OnceLock::new(),
            loaded: HashMap::new(),
            signature_mode,
            trusted_publisher_keys,
        };

        host.discover()?;
        Ok(host)
    }

    /// The plugins root, opening it if no package operation has yet.
    fn root(&self) -> Result<&recovery::Root, PluginError> {
        opened_root(&self.recovery_root, &self.plugins_dir)
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
        self.loaded = self.discovered()?;
        Ok(())
    }

    /// The packages discovery admits from the plugins directory as it is now.
    fn discovered(&self) -> Result<HashMap<String, LoadedPlugin>, PluginError> {
        self.discovered_named(None)
    }

    /// What [`Self::discovered`] admits, restricted to the entries whose
    /// manifest names `only` when it is given. Every check after a manifest is
    /// read is keyed by that manifest's name, so skipping the other names
    /// leaves the outcome for `only` as full discovery decides it, without
    /// reading any other package's component.
    fn discovered_named(
        &self,
        only: Option<&str>,
    ) -> Result<HashMap<String, LoadedPlugin>, PluginError> {
        let mut loaded = HashMap::new();
        if !self.plugins_dir.exists() {
            return Ok(loaded);
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
                    if only.is_some_and(|only| manifest.name != only) {
                        continue;
                    }
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
                            if loaded.remove(&manifest.name).is_some() {
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
                            #[cfg(all(test, unix))]
                            discovery_step(DiscoveryStep::BeforeComponentAdmission);
                            let component = match admit_component(&path, &manifest, &manifest_toml)
                            {
                                Ok(component) => component,
                                Err(e) => {
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to executable artifact admission failure");
                                    continue;
                                }
                            };
                            loaded.insert(
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

        Ok(loaded)
    }

    /// What discovery admits under `name` from the plugins directory as it is
    /// now, judged as discovery would judge it, without reading any other
    /// package. [`set_loaded`] records it.
    fn rediscovered(&self, name: &str) -> Result<Option<LoadedPlugin>, PluginError> {
        Ok(self.discovered_named(Some(name))?.remove(name))
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
        self.admit(source, AdmissionTarget::NewPackage)
    }

    /// Admit a source as the replacement for the installed package `name`.
    ///
    /// This is [`Self::admit_source`] with the identity decided the other way
    /// round: `name` must be loaded, which is checked before the source is
    /// opened, and the source's manifest must name it, which is checked before
    /// the signature and before the component is read. Everything else, from
    /// signature policy to the one confined read of the component, is the same
    /// admission a new package gets.
    pub fn admit_update(&self, name: &str, source: &str) -> Result<AdmittedSource, PluginError> {
        if !self.loaded.contains_key(name) {
            return Err(PluginError::NotFound(name.to_string()));
        }
        self.admit(source, AdmissionTarget::Replacement(name))
    }

    fn admit(
        &self,
        source: &str,
        target: AdmissionTarget<'_>,
    ) -> Result<AdmittedSource, PluginError> {
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

        let source_dir = manifest_source_dir(&manifest_path)?;
        let dir = Dir::open_ambient_dir(&source_dir, cap_std::ambient_authority())?;
        let filename = manifest_path
            .file_name()
            .ok_or_else(|| PluginError::InvalidManifest("no manifest filename".into()))?;
        let (manifest, manifest_toml, component) =
            self.admit_open_directory(&dir, &source_dir, filename, target)?;

        Ok(AdmittedSource {
            manifest,
            manifest_toml,
            source_dir,
            component,
        })
    }

    fn admit_open_directory(
        &self,
        dir: &Dir,
        display: &Path,
        filename: &std::ffi::OsStr,
        target: AdmissionTarget<'_>,
    ) -> Result<(PluginManifest, String, Option<AdmittedComponent>), PluginError> {
        let manifest_toml = dir.read_to_string(filename)?;
        let manifest: PluginManifest = toml::from_str(&manifest_toml)?;
        validate_manifest_shape_in(&manifest, dir, display)?;
        // Decide the package identity before the signature check and before
        // the component is read: nothing downstream runs for a package the
        // host has already decided not to accept.
        match target {
            AdmissionTarget::NewPackage if self.loaded.contains_key(&manifest.name) => {
                return Err(PluginError::AlreadyLoaded(manifest.name));
            }
            AdmissionTarget::Replacement(name) if manifest.name != name => {
                return Err(PluginError::InvalidManifest(format!(
                    "the update source for '{name}' is the package '{}'",
                    manifest.name
                )));
            }
            AdmissionTarget::NewPackage | AdmissionTarget::Replacement(_) => {}
        }
        self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest)?;
        validate_manifest_config(&manifest)?;
        let component = admit_component_in(dir, &manifest)?;
        Ok((manifest, manifest_toml, component))
    }

    /// Install a source admitted by [`Self::admit_source`].
    ///
    /// Persists the manifest bytes that were parsed and signature-checked and
    /// the component bytes that were admitted, so the file a verifier checked
    /// is the file the daemon will load. The `skills/` subtree of a
    /// skill-capable package is copied from the source directory as before; it
    /// is data, not executable code.
    ///
    /// Before the package is staged, the abandoned `installing` stages of the
    /// same name, whether an install or an update left them, are cleared once
    /// no process holds their leases. A stage whose lease is held is left
    /// alone, as is a legacy PID-named stage, whose owner cannot be proved,
    /// and a stage that cannot be cleared is skipped. A source inside one of
    /// that name's stages is refused before any is cleared, as
    /// [`PluginError::SourceInStage`]: its `skills/` tree is still to be
    /// copied, and clearing the stage would delete it.
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

        #[cfg(test)]
        recovery::pause("install-before-lock");
        let root = opened_root(&self.recovery_root, &self.plugins_dir)?;
        let _guard = root.lock()?;
        let dest_dir = self.plugins_dir.join(&manifest.name);
        match root.dir.symlink_metadata(&manifest.name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(PluginError::UnadmittedPackage {
                    name: manifest.name.clone(),
                    reason: if root.dir.symlink_metadata(&manifest.name)?.is_dir() {
                        "an existing directory occupies the destination".into()
                    } else {
                        "it is not a directory".into()
                    },
                });
            }
        }
        // Admission kept the manifest and component, but `skills/` is still
        // copied from the source. A source inside a stage of this name, such
        // as the package a failed install reports keeping, would be deleted by
        // the sweep below before that copy, so the install refuses first.
        if let Some(stage) = self.stage_holding(root, &manifest.name, &source_dir)? {
            return Err(PluginError::SourceInStage {
                path: stage.display().to_string(),
            });
        }
        // The abandoned `installing` stages of this name, whether an install
        // or an update left them, are cleared first, as `remove` clears them:
        // only those whose lease no process holds. A stage that cannot be
        // cleared is hidden and inert, so it is skipped and never stands in
        // the way of the install or of clearing the others; a changed
        // namespace still refuses.
        if let Err(error @ PluginError::NamespaceChanged(_)) =
            self.sweep_stale_staging(&manifest.name, true)
        {
            return Err(error);
        }
        let tx = root.transaction(&manifest.name, "installing")?;
        tx.dir.create_dir(recovery::PACKAGE)?;
        let package = tx.dir.open_dir(recovery::PACKAGE)?;
        let staged = write_package(
            &package,
            &manifest,
            &manifest_toml,
            &source_dir,
            component.as_ref(),
        )
        .and_then(|()| check_staged_skills(&manifest, &package));
        if let Err(error) = staged {
            clear_unpublished_stage(&self.plugins_dir, root, tx, package)
                .map_err(|failure| failure.error)?;
            return Err(error);
        }
        // Unix permits publishing an open directory: keep this exact staged
        // generation for cleanup if the move is unsupported. Windows needs
        // the directory closed before publication and uses the closed path.
        #[cfg(unix)]
        let package = Some(package);
        #[cfg(not(unix))]
        let package = {
            drop(package);
            None
        };
        // Publication never replaces an entry that holds files, even one an
        // unrelated writer put there; see `recovery::rename_new`.
        if let Err(error) = tx.publish(root, &manifest.name) {
            let path = root.retained_path(&tx);
            return Err(match error {
                // Without a rename that refuses an existing destination no
                // retry can publish, so the copy this install staged is cleared
                // as a failed write clears it, instead of one being kept for
                // every attempt.
                PluginError::Io(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                    #[cfg(test)]
                    recovery::pause("install-unsupported-before-cleanup");
                    cleanup_after_unsupported_publish(&self.plugins_dir, root, tx, package, error)
                }
                error => PluginError::RecoveryRetained {
                    path,
                    reason: error.to_string(),
                },
            });
        }
        drop(package);
        #[cfg(test)]
        recovery::pause("install-published");
        // The package is installed. Only the stage's lease and entry are left,
        // and the sweep of abandoned stages clears or reports an entry left
        // behind, so failing to finish the stage does not fail the install. A
        // namespace change still refuses.
        if let Err(error @ PluginError::NamespaceChanged(_)) = tx.finish(root) {
            return Err(error);
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
    ///
    /// For a loaded plugin, the directory it was loaded from, which need not be
    /// named `name`, is claimed under the package lock and deleted through the
    /// claim once it proves to hold the generation this host loaded; a claimed
    /// generation that does not is put back and refused as
    /// [`PluginError::NamespaceChanged`]. A claim this cannot settle, because
    /// that put-back or the delete fails, is reported as
    /// [`PluginError::RecoveryRetained`]. The claim records only `name`, so the
    /// next remove of `name` puts what it holds back at `name`, not in the
    /// directory it came from, and refuses while something occupies `name`.
    /// Claiming needs the atomic no-replace move install and recovery use, and
    /// refuses where the platform or filesystem has none. For a name the
    /// host did not load, `remove` is the recovery path for an interrupted
    /// install: it claims and validates the exact directory generation and
    /// cleans only staging whose protocol lease proves abandonment. Ambiguous
    /// legacy staging is retained. The final directory must be empty, or hold
    /// a `manifest.toml` naming `name` and nothing an install never writes,
    /// with admission rejecting its own contents rather than its signature.
    /// That includes a package this host cannot accept as written, such as one
    /// whose component exceeds the admission size limit. Anything else at the
    /// name is left untouched and reported as
    /// [`PluginError::UnadmittedPackage`] with the reason: a symlink or file, a
    /// directory holding files but no manifest, one holding anything an install
    /// never writes or whose manifest names another package, one that cannot be
    /// identified, inspected, or listed, a loaded package's directory, a
    /// package admission accepts, and one this host rejects for its signature
    /// policy. A refusal
    /// restores the claimed package without replacing a concurrent occupant
    /// that holds files; if restoration cannot complete, `RecoveryRetained`
    /// names the retained transaction and a later remove retries it.
    pub fn remove(&mut self, name: &str) -> Result<(), PluginError> {
        self.remove_with_report(name).map(|_| ())
    }

    /// Removal result includes ambiguous staging paths that were deliberately
    /// retained. Callers displaying a recovery result must surface these paths.
    pub fn remove_with_report(&mut self, name: &str) -> Result<Vec<PathBuf>, PluginError> {
        #[cfg(test)]
        recovery::pause("remove-before-lock");
        let root = opened_root(&self.recovery_root, &self.plugins_dir)?;
        let _guard = root.lock()?;
        if let Some(installed) = self.loaded.get(name) {
            // The directory the package was loaded from, which need not be
            // named `name`, is claimed, checked to hold the generation this
            // host loaded, and deleted through the claim, as an update
            // replaces it. Nothing at `name` is touched unless the package was
            // loaded from there.
            let entry = package_entry(&self.plugins_dir, &installed.plugin_dir)?;
            let held = root.transaction(name, "recovering")?;
            match held.claim(root, &entry) {
                Ok(()) => {}
                // Already gone, so nothing is left to delete.
                Err(PluginError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.loaded.remove(name);
                    finish_naming_entry(&self.plugins_dir, root, held)?;
                    return Ok(Vec::new());
                }
                Err(error) => {
                    let _ = held.finish(root);
                    return Err(error);
                }
            }
            #[cfg(test)]
            recovery::pause("loaded-after-claim");
            if let Err(error) = self.claimed_is_loaded(&held, name) {
                // Not the generation this host loaded: it goes back where it
                // was. One that cannot stays in the claim. The claim records
                // only `name`, so the next remove of `name` puts it back at
                // `name`, not where it came from, and refuses while something
                // occupies `name`.
                return Err(match held.publish(root, &entry) {
                    Ok(()) => {
                        let _ = held.finish(root);
                        error
                    }
                    Err(restore) => {
                        self.loaded.remove(name);
                        PluginError::RecoveryRetained {
                            path: root.retained_path(&held),
                            reason: format!("{error}; moving it back: {restore}"),
                        }
                    }
                });
            }
            self.loaded.remove(name);
            return match delete_claimed(&self.plugins_dir, root, held)? {
                None => Ok(Vec::new()),
                Some((left, reason)) => Err(PluginError::RecoveryRetained {
                    path: left.display().to_string(),
                    reason,
                }),
            };
        }
        crate::instance::validate_package_name(name)
            .map_err(|_| PluginError::NotFound(name.into()))?;
        let finished = self.retry_claims(name)?;
        let metadata = match std::fs::symlink_metadata(self.plugins_dir.join(name)) {
            Ok(metadata) => metadata,
            // What an earlier remove had begun deleting is gone now. That remove
            // had claimed and judged a final generation, so the staging sweep's
            // precondition holds.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && finished => {
                return self.remove_stale_staging(name);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PluginError::NotFound(name.into()));
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(reason) = self.kept_occupant(&self.plugins_dir.join(name), &metadata) {
            return Err(PluginError::UnadmittedPackage {
                name: name.into(),
                reason,
            });
        }
        let selected_id =
            same_file::Handle::from_path(self.plugins_dir.join(name)).map_err(|error| {
                PluginError::UnadmittedPackage {
                    name: name.into(),
                    reason: format!("it cannot be listed or cannot be inspected ({error})"),
                }
            })?;
        #[cfg(test)]
        recovery::pause("before-claim");
        let tx = root.transaction(name, "recovering")?;
        #[cfg(test)]
        recovery::pause("claim-created");
        tx.claim(root, name)?;
        #[cfg(test)]
        recovery::pause("after-claim");
        let claimed = match tx.dir.open_dir(recovery::PACKAGE) {
            Ok(dir) => dir,
            Err(error) => {
                return self.restore_refused(
                    name,
                    tx,
                    kept(
                        name,
                        format!("it cannot be listed or cannot be inspected ({error})"),
                    ),
                );
            }
        };
        let claimed_id = match claimed_identity(&claimed) {
            Ok(id) => id,
            Err(error) => {
                drop(claimed);
                return self.restore_refused(
                    name,
                    tx,
                    kept(
                        name,
                        format!("it cannot be listed or cannot be inspected ({error})"),
                    ),
                );
            }
        };
        let same_generation = selected_id == claimed_id;
        drop(claimed_id);
        drop(selected_id);
        if !same_generation {
            drop(claimed);
            return self.restore_refused(name, tx, PluginError::NamespaceChanged(name.into()));
        }
        if let Err(error) = self.recovery_verdict(name, &claimed) {
            drop(claimed);
            return self.restore_refused(name, tx, error);
        }
        // Stages are handled only after a real final generation was claimed and
        // judged recoverable. No-final calls never sweep unrelated stages.
        let retained =
            self.remove_stale_staging(name)
                .map_err(|error| PluginError::RecoveryRetained {
                    path: root.retained_path(&tx),
                    reason: error.to_string(),
                })?;
        #[cfg(test)]
        recovery::pause("before-delete");
        root.check()
            .map_err(|error| PluginError::RecoveryRetained {
                path: root.retained_path(&tx),
                reason: error.to_string(),
            })?;
        // Admission is about these bytes, not a verdict retained across stage IO.
        let empty = match self.recovery_verdict(name, &claimed) {
            Ok(empty) => empty,
            Err(error) => {
                drop(claimed);
                return self.restore_refused(name, tx, error);
            }
        };
        #[cfg(test)]
        recovery::pause("after-verdict");
        if !empty {
            // From here the delete is committed: a process that takes this
            // claim over after a stop finishes it instead of putting back what
            // is left. An empty claim needs no mark, because its removal below
            // is a single step.
            tx.mark_deleting()
                .map_err(|error| PluginError::RecoveryRetained {
                    path: root.retained_path(&tx),
                    reason: error.to_string(),
                })?;
            #[cfg(test)]
            recovery::pause("delete-marked");
            recovery::clear_owned(&claimed).map_err(|error| PluginError::RecoveryRetained {
                path: root.retained_path(&tx),
                reason: error.to_string(),
            })?;
        }
        drop(claimed);
        // Empty-only, so an empty claim that gained an entry since its verdict
        // is kept.
        tx.dir
            .remove_dir(recovery::PACKAGE)
            .and_then(|()| tx.remove_mark().map_err(std::io::Error::other))
            .map_err(|error| PluginError::RecoveryRetained {
                path: root.retained_path(&tx),
                reason: error.to_string(),
            })?;
        #[cfg(test)]
        recovery::pause("package-deleted");
        finish_naming_entry(&self.plugins_dir, root, tx)?;
        Ok(retained)
    }

    fn recovery_verdict(&self, name: &str, dir: &Dir) -> Result<bool, PluginError> {
        match dir.symlink_metadata("manifest.toml") {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if dir.entries()?.next().transpose()?.is_none() {
                    return Ok(true);
                }
                return Err(kept(
                    name,
                    "it holds files but no manifest.toml, so no interrupted install left it",
                ));
            }
            Err(error) => {
                return Err(kept(
                    name,
                    format!("its manifest.toml cannot be inspected ({error})"),
                ));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(kept(name, "its manifest.toml is not a regular file"));
            }
            Ok(_) => {}
        }
        // Exactly the same manifest, signature, schema and payload admission as
        // install, but all reads resolve from the claimed directory capability.
        match self.admit_open_directory(
            dir,
            Path::new("claimed package"),
            std::ffi::OsStr::new("manifest.toml"),
            AdmissionTarget::NewPackage,
        ) {
            Ok(_) => Err(kept(name, "admission accepts this package")),
            Err(error) if is_structural_admission_failure(&error) => {
                dir.entries()
                    .map_err(|error| kept(name, format!("it cannot be listed ({error})")))?;
                if let Some(reason) = beyond_install_footprint(dir, name)? {
                    return Err(kept(name, reason));
                }
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn restore_refused<T>(
        &self,
        name: &str,
        tx: recovery::Transaction,
        reason: PluginError,
    ) -> Result<T, PluginError> {
        let root = self.root()?;
        // Recovery's own reason reads as written; any other error keeps its
        // description.
        let reason = match reason {
            PluginError::UnadmittedPackage { reason, .. } => reason,
            other => other.to_string(),
        };
        #[cfg(test)]
        recovery::pause("before-restore");
        if let Err(error) = tx.publish(root, name) {
            return Err(PluginError::RecoveryRetained {
                path: root.retained_path(&tx),
                reason: format!("{reason}; restoration refused: {error}"),
            });
        }
        // The package is back at its name, so only the entry can be left, and
        // the error still says why the package was kept.
        if let Err(error) = finish_naming_entry(&self.plugins_dir, root, tx) {
            return Err(match error {
                PluginError::RecoveryRetained { path, reason: left } => {
                    PluginError::RecoveryRetained {
                        path,
                        reason: format!("{reason}; {left}"),
                    }
                }
                other => other,
            });
        }
        Err(PluginError::UnadmittedPackage {
            name: name.into(),
            reason,
        })
    }

    /// Settle the claims an earlier remove of `name` left behind: finish a
    /// delete it had begun, or put back a package it had not. Returns whether
    /// it finished a delete.
    fn retry_claims(&self, name: &str) -> Result<bool, PluginError> {
        let root = self.root()?;
        let mut finished = false;
        for entry in root.dir.entries()? {
            let entry = entry?;
            let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !recovery::is_transaction(&entry_name, name, "recovering") {
                continue;
            }
            let Some(tx) = root.reopen_transaction(entry_name.clone())? else {
                if root.remove_empty_entry(&entry_name)? {
                    continue;
                }
                return Err(PluginError::RecoveryRetained {
                    path: self.plugins_dir.join(entry_name).display().to_string(),
                    reason: "recovery transaction ownership is unavailable".into(),
                });
            };
            // A delete the earlier remove had begun is finished through the
            // claim, never put back for a second verdict on what it left.
            let deleting = tx
                .is_deleting()
                .map_err(|error| PluginError::RecoveryRetained {
                    path: root.retained_path(&tx),
                    reason: error.to_string(),
                })?;
            if deleting {
                tx.finish_delete()
                    .map_err(|error| PluginError::RecoveryRetained {
                        path: root.retained_path(&tx),
                        reason: error.to_string(),
                    })?;
                finish_naming_entry(&self.plugins_dir, root, tx)?;
                finished = true;
                continue;
            }
            match tx.dir.symlink_metadata(recovery::PACKAGE) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    finish_naming_entry(&self.plugins_dir, root, tx)?;
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    // Restore before restarting recovery; crash retry never
                    // interprets hidden placement as proof of defective contents.
                    if let Err(error) = tx.publish(root, name) {
                        return Err(PluginError::RecoveryRetained {
                            path: root.retained_path(&tx),
                            reason: error.to_string(),
                        });
                    }
                    finish_naming_entry(&self.plugins_dir, root, tx)?;
                }
            }
        }
        Ok(finished)
    }

    /// Why whatever is at `path` (described by its `symlink_metadata`) is not
    /// recovery's to delete, or `None` for a real directory no loaded package
    /// lives in.
    fn kept_occupant(&self, path: &Path, metadata: &std::fs::Metadata) -> Option<String> {
        if metadata.file_type().is_symlink() {
            return Some("it is a symlink, which is never followed".to_string());
        }
        if !metadata.is_dir() {
            return Some("it is not a directory".to_string());
        }
        // A loaded package's files are never recovery's, whatever its
        // manifest reads by now. Compared by file identity, not spelling: on a
        // case-insensitive file system a slug reaches a directory spelled in
        // another case. A directory that cannot be identified is kept.
        let target = match directory_identity(path) {
            Ok(target) => target,
            Err(error) => return Some(format!("it cannot be identified ({error})")),
        };
        self.loaded
            .values()
            .find(|plugin| {
                directory_identity(&plugin.plugin_dir).is_ok_and(|loaded| loaded == target)
            })
            .map(|plugin| format!("plugin '{}' is loaded from it", plugin.manifest.name))
    }

    fn remove_stale_staging(&self, name: &str) -> Result<Vec<PathBuf>, PluginError> {
        self.sweep_stale_staging(name, false)
    }

    /// Clear the abandoned `installing` stages of `name`, whether an install
    /// or an update left them, and return the stages kept for the caller to
    /// report. The first error ends the sweep, unless `keep_going`: install
    /// then skips a stage it cannot clear and goes on to the rest, and only a
    /// namespace change still ends the sweep at once.
    fn sweep_stale_staging(
        &self,
        name: &str,
        keep_going: bool,
    ) -> Result<Vec<PathBuf>, PluginError> {
        let root = self.root()?;
        let mut retained = Vec::new();
        for entry in root.dir.entries()? {
            match self.sweep_stage(root, name, entry) {
                Ok(Some(kept)) => retained.push(kept),
                Ok(None) => {}
                Err(error) if keep_going && !matches!(error, PluginError::NamespaceChanged(_)) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "plugin": name,
                                "error": error.to_string(),
                            })),
                        "leaving an abandoned install stage that could not be cleared"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(retained)
    }

    /// The `installing` stage of `name` that `source_dir` lies in, if one
    /// does. Compared by directory identity, not spelling, so a source reached
    /// through a symlink, a `..`, or another path to the plugins directory is
    /// still found in its stage. The source is resolved only when the name has
    /// a stage.
    fn stage_holding(
        &self,
        root: &recovery::Root,
        name: &str,
        source_dir: &Path,
    ) -> Result<Option<PathBuf>, PluginError> {
        let mut stages = Vec::new();
        for entry in root.dir.entries()? {
            let entry_name = entry?.file_name();
            if let Some(entry_name) = entry_name.to_str()
                && recovery::is_transaction(entry_name, name, "installing")
            {
                stages.push(self.plugins_dir.join(entry_name));
            }
        }
        if stages.is_empty() {
            return Ok(None);
        }
        let holders = std::fs::canonicalize(source_dir)?
            .ancestors()
            .map(directory_identity)
            .collect::<std::io::Result<Vec<_>>>()?;
        for stage in stages {
            if holders.contains(&directory_identity(&stage)?) {
                return Ok(Some(stage));
            }
        }
        Ok(None)
    }

    /// Clear `entry` if it is an abandoned leased stage of `name`. Returns
    /// its path when it is a stage that is kept: a legacy PID-named stage,
    /// whose owner cannot be proved, or one with no lease that is not empty.
    fn sweep_stage(
        &self,
        root: &recovery::Root,
        name: &str,
        entry: std::io::Result<cap_std::fs::DirEntry>,
    ) -> Result<Option<PathBuf>, PluginError> {
        let entry = entry?;
        let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
            return Ok(None);
        };
        if !entry_name.starts_with(&staging_prefix(name)) {
            return Ok(None);
        }
        if !recovery::is_transaction(&entry_name, name, "installing") {
            let legacy = entry_name
                .strip_prefix(&staging_prefix(name))
                .is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                });
            return Ok(legacy.then(|| self.plugins_dir.join(entry_name)));
        }
        #[cfg(test)]
        sweep_fault::take()?;
        let Some(tx) = root.reopen_transaction(entry_name.clone())? else {
            if !root.remove_empty_entry(&entry_name)? {
                return Ok(Some(self.plugins_dir.join(entry_name)));
            }
            return Ok(None);
        };
        // A protocol stage's held lease is the ownership fact. Never create
        // a lease in an unknown directory and call that proof of abandonment.
        match tx.dir.open_dir(recovery::PACKAGE) {
            Ok(package) => {
                #[cfg(test)]
                recovery::pause("before-stage-delete");
                root.check()?;
                recovery::clear_owned(&package)?;
                drop(package);
                tx.dir.remove_dir(recovery::PACKAGE)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        tx.finish(root)?;
        Ok(None)
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

/// Updates: replacing an installed package with an admitted source, and
/// recovering what an interrupted replacement left.
///
/// Every step runs under the package lock that serializes install, remove and
/// recovery across hosts. The replacement is built in a leased stage, as
/// install builds a package, and the installed generation is claimed into a
/// leased transaction of its own before the replacement is published in its
/// place. Both moves are renames that never overwrite, and a claimed generation
/// is deleted only through its own handle. Discovery ignores the hidden
/// transaction directories, so neither a half-built replacement nor a claimed
/// generation is ever loaded, and a lease that outlived its process is how
/// recovery tells an interrupted update from one still running.
impl PluginHost {
    /// Replace the installed package that `admitted` names.
    ///
    /// `admitted` comes from [`Self::admit_update`]. The replacement is staged
    /// as [`Self::install_admitted`] stages a new package, and the staged skill
    /// bundle of a skill-capable package is validated as discovery will
    /// validate it: staging copies no symbolic links, so a bundle admission
    /// accepted can stage incomplete. Only then is the installed generation
    /// claimed. If its manifest text or admitted component differs from the
    /// generation this host loaded, it is published back and the update
    /// refused, because the caller compared authority against the generation
    /// it loaded. The replacement is then published in the directory the
    /// installed package was discovered in, and if that fails the claimed
    /// generation is published back. So on every error except
    /// [`PluginError::ReplacementInterrupted`] the installed package is exactly
    /// as it was.
    ///
    /// Configuration and durable state are not touched: they belong to the
    /// instance identity, which does not include the version.
    ///
    /// # Preconditions
    ///
    /// This method does not compare authority. The runtime grants a plugin
    /// what its manifest requests, so before calling it the caller must compare
    /// the authority `admitted` requests (its permissions, capabilities,
    /// `provides` ids and publisher) with the generation this host loaded, and
    /// have the operator accept any increase. What this method guarantees is
    /// that the generation it replaces is the one that comparison was made
    /// against.
    ///
    /// # Errors
    ///
    /// - [`PluginError::NotFound`] when no package of that name is loaded.
    /// - [`PluginError::NamespaceChanged`] when the installed package is not
    ///   the generation this host loaded, or the plugins directory changed.
    ///   The installed package is put back first.
    /// - An I/O error when the package lock is still held after 60 seconds, or
    ///   when staging, claiming or publishing fails, and
    ///   [`PluginError::InvalidManifest`] when the staged skill bundle is not
    ///   what discovery would load. The installed package is untouched or put
    ///   back.
    /// - [`PluginError::ReplacementInterrupted`] when the claimed generation
    ///   could not be put back either. It names where that generation is
    ///   preserved, and this host no longer holds the package as loaded.
    pub fn update_admitted(
        &mut self,
        admitted: AdmittedSource,
    ) -> Result<ReplacedPackage, PluginError> {
        let AdmittedSource {
            manifest,
            manifest_toml,
            source_dir,
            component,
        } = admitted;

        // Re-checked here as well: the package may have been removed between
        // admission and replacement.
        let Some(installed) = self.loaded.get(&manifest.name) else {
            return Err(PluginError::NotFound(manifest.name));
        };
        let package_dir = installed.plugin_dir.clone();
        let previous_version = installed.manifest.version.clone();
        let entry = package_entry(&self.plugins_dir, &package_dir)?;

        let root = opened_root(&self.recovery_root, &self.plugins_dir)?;
        let _guard = root.lock()?;
        // The replacement is staged as install stages a package, and the
        // handle it was written through is kept for cleanup, so a directory
        // substituted at `package` meanwhile is never the one cleared.
        let stage = root.transaction(&manifest.name, "installing")?;
        if let Err(error) = stage.dir.create_dir(recovery::PACKAGE) {
            discard(root, stage, None);
            return Err(error.into());
        }
        let package = match stage.dir.open_dir(recovery::PACKAGE) {
            Ok(package) => package,
            Err(error) => {
                discard(root, stage, None);
                return Err(error.into());
            }
        };
        let staged = write_package(
            &package,
            &manifest,
            &manifest_toml,
            &source_dir,
            component.as_ref(),
        )
        .and_then(|()| check_staged_skills(&manifest, &package));
        if let Err(error) = staged {
            discard(root, stage, Some(package));
            return Err(error);
        }
        #[cfg(test)]
        recovery::pause("update-staged");

        let held = match root.transaction(&manifest.name, "replacing") {
            Ok(held) => held,
            Err(error) => {
                discard(root, stage, Some(package));
                return Err(error);
            }
        };
        #[cfg(test)]
        recovery::pause("update-before-claim");
        if let Err(error) = held.claim(root, &entry) {
            let _ = held.finish(root);
            discard(root, stage, Some(package));
            return Err(error);
        }
        #[cfg(test)]
        recovery::pause("update-claimed");
        if let Err(error) = self.claimed_is_loaded(&held, &manifest.name) {
            discard(root, stage, Some(package));
            let refused = put_back(root, &manifest.name, &entry, held, error);
            if matches!(refused, PluginError::ReplacementInterrupted { .. }) {
                self.loaded.remove(&manifest.name);
            }
            return Err(refused);
        }

        // Unix permits publishing an open directory, so the staged handle is
        // kept for cleanup if publication fails. Windows needs the directory
        // closed before publication, and its cleanup reopens it through the
        // stage, still behind the namespace checks.
        #[cfg(unix)]
        let package = Some(package);
        #[cfg(not(unix))]
        let package = {
            drop(package);
            None
        };
        #[cfg(test)]
        replace_hook::run(&self.plugins_dir, &stage.entry, &held.entry);
        if let Err(error) = stage.publish(root, &entry) {
            discard(root, stage, package);
            let refused = put_back(root, &manifest.name, &entry, held, error);
            if matches!(refused, PluginError::ReplacementInterrupted { .. }) {
                self.loaded.remove(&manifest.name);
            }
            return Err(refused);
        }
        drop(package);
        #[cfg(test)]
        recovery::pause("update-published");
        // The stage holds nothing but its lease now. One left behind is swept
        // with the other abandoned stages.
        let _ = stage.finish(root);
        // The replacement is in place, whatever then happens to the replaced
        // generation, so even a namespace change is reported with it.
        let held_at = self.plugins_dir.join(&held.entry);
        let leftover = delete_claimed(&self.plugins_dir, root, held)
            .unwrap_or_else(|error| Some((held_at, error.to_string())));

        self.loaded.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
                manifest_toml,
                plugin_dir: package_dir,
                component,
            },
        );
        Ok(ReplacedPackage {
            previous_version,
            leftover,
        })
    }

    /// Put back a package an interrupted update left claimed, or delete the
    /// generations finished updates left behind.
    ///
    /// Runs under the package lock and judges only `replacing` transactions
    /// whose lease it can take: a lease still held belongs to a process that
    /// has not finished, and its claimed generation is never touched. Whether
    /// the package is installed is judged from the plugins directory as it is
    /// under the lock, not from this host's earlier view, which can predate
    /// the update that left the claims. Only `name` is judged again, and every
    /// other package keeps this host's earlier view, so a batch that recovers
    /// each of its names does not admit every installed package once per name.
    /// When it is installed, every claimed
    /// generation of it is out of date, so each is marked for deletion and
    /// deleted through its handle; one that cannot be is reported and left.
    /// When it is not, the one generation an interrupted update claimed is
    /// published back to `<plugins_dir>/<name>`, so the restored package is
    /// admitted only if the configured policy accepts it, like any other. An
    /// installed package of the name takes precedence over any claimed
    /// generation. Several claimed generations, or anything already at the
    /// name, are reported and left as found; a generation marked for deletion
    /// is never put back; and a claim that holds neither is finished. Stages that
    /// interrupted updates of `name` left are swept as `remove` sweeps them.
    /// A batch that did not name the package calls
    /// [`Self::sweep_interrupted_update`] instead, which never puts one back.
    pub fn recover_interrupted_update(
        &mut self,
        name: &str,
    ) -> Result<UpdateRecovery, PluginError> {
        self.recover_update(name, true)
    }

    /// [`Self::recover_interrupted_update`] for a batch that did not name the
    /// package. Generations finished updates left are deleted the same way,
    /// but a package an interrupted update displaced is left where it is and
    /// reported as [`UpdateRecovery::Displaced`], so only recovering it by
    /// name puts it back. That is judged under the lock, so a package
    /// displaced after the batch chose its names is left in place too.
    pub fn sweep_interrupted_update(&mut self, name: &str) -> Result<UpdateRecovery, PluginError> {
        self.recover_update(name, false)
    }

    /// [`Self::recover_interrupted_update`], which puts a displaced package
    /// back only when `restore` is set.
    fn recover_update(&mut self, name: &str, restore: bool) -> Result<UpdateRecovery, PluginError> {
        if crate::instance::validate_package_name(name).is_err() {
            return Ok(UpdateRecovery::Nothing);
        }
        #[cfg(test)]
        recovery::pause("recovery-before-lock");
        let root = opened_root(&self.recovery_root, &self.plugins_dir)?;
        let _guard = root.lock()?;
        let claims = self.abandoned_claims(name)?;
        // A stage that cannot be swept is hidden and inert, so it never stands
        // in the way of recovering the package itself; a changed namespace
        // still refuses.
        if let Err(error @ PluginError::NamespaceChanged(_)) = self.remove_stale_staging(name) {
            return Err(error);
        }
        if claims.is_empty() {
            // An update another process finished while this one waited for the
            // lock has changed what is installed.
            let current = self.rediscovered(name)?;
            set_loaded(&mut self.loaded, name, current);
            return Ok(UpdateRecovery::Nothing);
        }
        let current = self.rediscovered(name)?;
        set_loaded(&mut self.loaded, name, current);
        let installed = self.loaded.contains_key(name);

        let mut restorable = Vec::new();
        let mut outdated = Vec::new();
        for tx in claims {
            match claim_contents(&tx)? {
                ClaimContents::Package if !installed => restorable.push(tx),
                ClaimContents::Package | ClaimContents::Deleting => outdated.push(tx),
                ClaimContents::Empty => {
                    let _ = tx.finish(root);
                }
            }
        }
        let mut removed = 0;
        let mut kept = Vec::new();
        for tx in outdated {
            match delete_claimed(&self.plugins_dir, root, tx)? {
                None => removed += 1,
                Some(left) => kept.push(left),
            }
        }
        // Whatever recovery does next, the generations it could not delete
        // are reported with it.
        let held = match restorable.len() {
            0 if removed == 0 && kept.is_empty() => return Ok(UpdateRecovery::Nothing),
            0 => return Ok(UpdateRecovery::Swept { removed, kept }),
            _ if !restore => {
                return Ok(UpdateRecovery::Displaced {
                    displaced: restorable
                        .iter()
                        .map(|tx| PathBuf::from(root.retained_path(tx)))
                        .collect(),
                    kept,
                });
            }
            1 => restorable.remove(0),
            _ => {
                return Ok(UpdateRecovery::Ambiguous {
                    displaced: restorable
                        .iter()
                        .map(|tx| PathBuf::from(root.retained_path(tx)))
                        .collect(),
                    kept,
                });
            }
        };
        match root.dir.symlink_metadata(name) {
            Ok(_) => {
                return Ok(UpdateRecovery::Occupied {
                    displaced: PathBuf::from(root.retained_path(&held)),
                    occupant: self.plugins_dir.join(name),
                    kept,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        held.publish(root, name)?;
        let _ = held.finish(root);
        let current = self.rediscovered(name)?;
        set_loaded(&mut self.loaded, name, current);
        Ok(UpdateRecovery::Restored { kept })
    }

    /// Packages an interrupted update left claimed: sorted names that are not
    /// loaded and have an abandoned claim that holds a package.
    /// [`Self::recover_interrupted_update`] puts such a package back, or, with
    /// several claims or an occupied name, reports why it cannot. Runs under
    /// the package lock, so it never holds a claim's lease while a recovery
    /// judges it.
    pub fn displaced_packages(&self) -> Result<Vec<String>, PluginError> {
        let root = self.root()?;
        let _guard = root.lock()?;
        let mut current: Option<HashMap<String, LoadedPlugin>> = None;
        let mut names = Vec::new();
        for entry in root.dir.entries()? {
            let entry = entry?;
            let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(package) = recovery::transaction_package(&entry_name, "replacing") else {
                continue;
            };
            // Judged from the plugins directory under the lock, not from this
            // host's earlier view, as recovery judges it.
            if current.is_none() {
                current = Some(self.discovered()?);
            }
            if current
                .as_ref()
                .is_some_and(|loaded| loaded.contains_key(package))
            {
                continue;
            }
            let package = package.to_string();
            // Taking the lease, briefly, skips a claim an update still holds;
            // one that cannot be inspected is left to recovery to report.
            if let Ok(Some(tx)) = root.reopen_transaction(entry_name)
                && matches!(claim_contents(&tx), Ok(ClaimContents::Package))
            {
                names.push(package);
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// The `replacing` transactions of `name` whose lease this host can take:
    /// those an update left when it stopped. Under the package lock a
    /// cooperating update holds none, so a lease still held is reported
    /// rather than judged.
    fn abandoned_claims(&self, name: &str) -> Result<Vec<recovery::Transaction>, PluginError> {
        let root = self.root()?;
        let mut claims = Vec::new();
        for entry in root.dir.entries()? {
            let entry = entry?;
            let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !recovery::is_transaction(&entry_name, name, "replacing") {
                continue;
            }
            let Some(tx) = root.reopen_transaction(entry_name.clone())? else {
                if root.remove_empty_entry(&entry_name)? {
                    continue;
                }
                return Err(PluginError::RecoveryRetained {
                    path: self.plugins_dir.join(entry_name).display().to_string(),
                    reason: "update transaction ownership is unavailable".into(),
                });
            };
            claims.push(tx);
        }
        claims.sort_by(|a, b| a.entry.cmp(&b.entry));
        Ok(claims)
    }

    /// Whether the generation `held` claimed is the one this host loaded as
    /// `name`: the same manifest text, which carries all the authority the
    /// caller compared, and the same admitted component. Skill content is not
    /// compared; it was never admitted bytes.
    fn claimed_is_loaded(
        &self,
        held: &recovery::Transaction,
        name: &str,
    ) -> Result<(), PluginError> {
        let changed = || {
            PluginError::NamespaceChanged(format!(
                "the installed package '{name}' changed after this host loaded it"
            ))
        };
        let loaded = self.loaded.get(name).ok_or_else(changed)?;
        let claimed = held.dir.open_dir(recovery::PACKAGE)?;
        let same_manifest = claimed
            .read_to_string("manifest.toml")
            .is_ok_and(|text| text == loaded.manifest_toml);
        if !same_manifest {
            return Err(changed());
        }
        let component = admit_component_in(&claimed, &loaded.manifest).map_err(|_| changed())?;
        if component.as_ref().map(AdmittedComponent::bytes)
            != loaded.component.as_ref().map(AdmittedComponent::bytes)
        {
            return Err(changed());
        }
        Ok(())
    }
}

/// Publish a generation `held` claimed back to `entry`, and return the error
/// to report: `error` itself, or, when even that fails,
/// [`PluginError::ReplacementInterrupted`]. The generation then stays in its
/// transaction, whose lease is released on return, for recovery to put back,
/// and the caller no longer holds the package as loaded.
fn put_back(
    root: &recovery::Root,
    name: &str,
    entry: &str,
    held: recovery::Transaction,
    error: PluginError,
) -> PluginError {
    match held.publish(root, entry) {
        Ok(()) => {
            let _ = held.finish(root);
            error
        }
        Err(restore) => PluginError::ReplacementInterrupted {
            name: name.to_string(),
            preserved: PathBuf::from(root.retained_path(&held)),
            cause: format!("{error}; moving it back: {restore}"),
        },
    }
}

/// Mark a claimed generation for deletion, with the mark a remove sets once
/// it has judged a package incomplete, then delete it through its handle.
/// Once marked, recovery only ever finishes the delete and never puts the
/// generation back. Returns where it was left, and why, when it could not be
/// deleted, and refuses when the plugins directory itself changed.
fn delete_claimed(
    plugins_dir: &Path,
    root: &recovery::Root,
    held: recovery::Transaction,
) -> Result<Option<(PathBuf, String)>, PluginError> {
    let left = plugins_dir.join(&held.entry).join(recovery::PACKAGE);
    let marked = match held.is_deleting() {
        Ok(marked) => marked,
        Err(error) => return Ok(Some((left, error.to_string()))),
    };
    if !marked {
        match root.check().and_then(|()| held.mark_deleting()) {
            Ok(()) => {}
            Err(error @ PluginError::NamespaceChanged(_)) => return Err(error),
            Err(error) => return Ok(Some((left, error.to_string()))),
        }
    }
    match held.finish_delete() {
        Ok(()) => {
            let _ = held.finish(root);
            Ok(None)
        }
        Err(error) => Ok(Some((left, error.to_string()))),
    }
}

/// Clear the replacement an update staged and release its stage, with the
/// checks install's cleanup makes: the plugins root and the package lock are
/// checked before the clear, before unlinking the package directory and
/// before finishing the stage, and a stage whose namespace changed is left as
/// found. The clear goes through `package`, the handle the stage was written
/// through, so a directory substituted at `package` is never the one cleared.
/// `package` is `None` when the directory was never opened, or was closed so
/// that it could be published; it is then reopened through the stage's own
/// handle. A package directory that is already gone has nothing left to
/// unlink. A stage that cannot be cleared is left for the sweep of abandoned
/// stages.
fn discard(root: &recovery::Root, stage: recovery::Transaction, package: Option<Dir>) {
    let package = match package {
        Some(package) => Some(package),
        None => match stage.dir.open_dir(recovery::PACKAGE) {
            Ok(package) => Some(package),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return,
        },
    };
    if let Some(package) = package {
        if check_cleanup_namespace(root).is_err() || recovery::clear_owned(&package).is_err() {
            return;
        }
        drop(package);
        if check_cleanup_namespace(root).is_err() {
            return;
        }
        match stage.dir.remove_dir(recovery::PACKAGE) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return,
        }
    }
    if check_cleanup_namespace(root).is_ok() {
        let _ = stage.finish(root);
    }
}

/// Check the skill bundle a skill-capable package actually staged, as
/// discovery will check it: staging copies no symbolic links, while admission
/// follows them within the package, so a bundle admission accepted can stage
/// incomplete.
fn check_staged_skills(manifest: &PluginManifest, package: &Dir) -> Result<(), PluginError> {
    if !manifest.capabilities.contains(&PluginCapability::Skill) {
        return Ok(());
    }
    validate_skill_bundle(&manifest.name, package, Path::new("staged package")).map_err(
        |error| match error {
            PluginError::InvalidManifest(cause) => PluginError::InvalidManifest(format!(
                "the staged copy of '{}' is incomplete: {cause}. Staging does not copy symbolic links; replace any under skills/ with the files they point to",
                manifest.name
            )),
            other => other,
        },
    )
}

/// What a `replacing` transaction holds.
enum ClaimContents {
    /// A claimed generation, which recovery may put back.
    Package,
    /// A claim marked for deletion, with or without what is left of its
    /// generation, which recovery only ever deletes.
    Deleting,
    /// Neither: the claim was never made or has been undone.
    Empty,
}

/// What `tx` holds. A lookup that fails for any reason but absence says
/// nothing about the claim, so it is an error, never a verdict.
fn claim_contents(tx: &recovery::Transaction) -> Result<ClaimContents, PluginError> {
    if tx.is_deleting()? {
        Ok(ClaimContents::Deleting)
    } else if holds(&tx.dir, recovery::PACKAGE)? {
        Ok(ClaimContents::Package)
    } else {
        Ok(ClaimContents::Empty)
    }
}

/// Whether `dir` holds `entry`, of any type.
fn holds(dir: &Dir, entry: &str) -> Result<bool, PluginError> {
    match dir.symlink_metadata(entry) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// The name of a loaded package's directory in the plugins directory: the one
/// entry an update claims and publishes to.
fn package_entry(plugins_dir: &Path, package_dir: &Path) -> Result<String, PluginError> {
    package_dir
        .parent()
        .filter(|parent| *parent == plugins_dir)
        .and(package_dir.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            PluginError::NamespaceChanged(format!(
                "{} is not a package directory in {}",
                package_dir.display(),
                plugins_dir.display()
            ))
        })
}

/// Test-only seam: runs once between claiming the installed generation and
/// publishing its replacement, with the plugins directory and the entries of
/// the stage and of the claim, so a test can make the publication, the
/// publication back, or the deletion of the claimed generation fail.
#[cfg(test)]
mod replace_hook {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn FnOnce(&Path, &str, &str)>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn set(hook: impl FnOnce(&Path, &str, &str) + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(super) fn run(plugins_dir: &Path, stage: &str, claim: &str) {
        if let Some(hook) = HOOK.with(|slot| slot.borrow_mut().take()) {
            hook(plugins_dir, stage, claim);
        }
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

/// Admit the component of the package at `plugin_dir`, and prove that the
/// manifest describing it is one this package holds.
///
/// Discovery reads every package's manifest by pathname before it decides
/// which packages to admit, and admission then resolves the payload through a
/// confined handle of that package root. A republication in between would pair
/// the manifest that decided admission with another generation's bytes, so the
/// manifest is read again through that same handle and must match the text
/// admission was decided on. A different generation is refused; identical text
/// admits a payload the admitted policy already describes.
fn admit_component(
    plugin_dir: &Path,
    manifest: &PluginManifest,
    manifest_toml: &str,
) -> Result<Option<AdmittedComponent>, PluginError> {
    let Some(relative) = manifest.wasm_path.as_deref() else {
        return Ok(None);
    };
    let republished = || {
        PluginError::NamespaceChanged(format!(
            "plugin package changed between reading its manifest and admitting its component: {}",
            plugin_dir.display()
        ))
    };
    let confined = resolve_confined_wasm_path(plugin_dir, relative)?;
    let admitted_manifest = confined
        .at_relative(PathBuf::from("manifest.toml"))
        .and_then(|manifest| read_stable_file(&manifest))
        .map_err(|_| republished())?;
    if admitted_manifest != manifest_toml.as_bytes() {
        return Err(republished());
    }
    let bytes = read_stable_file(&confined)?;
    if let Some(expected) = manifest.wasm_sha256.as_deref() {
        signature::verify_payload_digest(&bytes, expected)?;
    }
    Ok(Some(AdmittedComponent::new(bytes)))
}

/// Admit the component of the package `dir` holds, reading it through that
/// handle: the source directory install opened, or the generation recovery
/// claimed. On Unix the payload is opened as a confined read opens one: a
/// symlink on the way is refused, a FIFO fails at once instead of blocking, and
/// the regular-file check refuses any other special file. Elsewhere a symlink on
/// the way is refused before the open.
fn admit_component_in(
    dir: &Dir,
    manifest: &PluginManifest,
) -> Result<Option<AdmittedComponent>, PluginError> {
    manifest
        .wasm_path
        .as_deref()
        .map(|relative| {
            validate_manifest_subpath("wasm_path", &manifest.name, relative)?;
            let file = open_payload_in(dir, relative)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(PluginError::InvalidManifest(
                    "WASM payload is not a regular file".into(),
                ));
            }
            let bytes = read_component_bytes(file, metadata.len())?;
            if let Some(expected) = manifest.wasm_sha256.as_deref() {
                signature::verify_payload_digest(&bytes, expected)?;
            }
            Ok(AdmittedComponent::new(bytes))
        })
        .transpose()
}

/// Open the payload at `relative` below `dir`: each directory without
/// following a symlink, then the payload itself without following one and
/// without blocking.
#[cfg(unix)]
fn open_payload_in(dir: &Dir, relative: &str) -> Result<std::fs::File, PluginError> {
    use rustix::io::Errno;
    use std::os::fd::AsFd;

    // Normal components only; `validate_manifest_subpath` refused the rest.
    let relative: PathBuf = Path::new(relative)
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .collect();
    let refused = |errno: Errno, _walked: &Path| match errno {
        Errno::NOENT => PluginError::NotFound(relative.display().to_string()),
        // A symlink or non-directory where a directory is expected fails the
        // no-follow directory open with ENOTDIR (ELOOP on older Linux kernels);
        // a symlink in place of the payload fails its no-follow open with ELOOP.
        Errno::LOOP | Errno::NOTDIR => PluginError::InvalidManifest(format!(
            "wasm_path contains a symlink or non-directory component: {}",
            relative.display()
        )),
        other => PluginError::Io(other.into()),
    };
    let (parent, leaf) = open_parent_below(dir.as_fd(), &relative, refused)?;
    let payload = open_payload_leaf(&parent, leaf).map_err(|errno| refused(errno, &relative))?;
    Ok(std::fs::File::from(payload))
}

/// Elsewhere `dir` resolves the path, after a check that no component on the
/// way is a symlink and that the payload is a regular file. Opening a
/// directory as a file fails there with an access error, which would read as
/// an I/O failure rather than as a payload that is not a file, as it does on
/// Unix.
#[cfg(not(unix))]
fn open_payload_in(dir: &Dir, relative: &str) -> Result<std::fs::File, PluginError> {
    let mut prefix = PathBuf::new();
    let mut is_file = false;
    for component in Path::new(relative).components() {
        prefix.push(component);
        let metadata = dir.symlink_metadata(&prefix).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                PluginError::NotFound(relative.into())
            } else {
                error.into()
            }
        })?;
        if metadata.is_symlink() {
            return Err(PluginError::InvalidManifest(
                "wasm_path contains a symlink".into(),
            ));
        }
        is_file = metadata.is_file();
    }
    if !is_file {
        return Err(PluginError::InvalidManifest(
            "WASM payload is not a regular file".into(),
        ));
    }
    Ok(dir.open(relative)?.into_std())
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

impl ConfinedPayload {
    /// Another file below the same, already-confined root.
    ///
    /// The root handle is retained, not reopened by pathname, so a read below
    /// the returned payload reaches the same generation as a read below `self`
    /// does, and confinement checks the replacement of a root that moved.
    fn at_relative(&self, relative: PathBuf) -> Result<ConfinedPayload, PluginError> {
        Ok(ConfinedPayload {
            root: self.root.clone(),
            relative,
            #[cfg(unix)]
            root_dir: self.root_dir.try_clone()?,
            // `same_file::Handle` has no clone of its own; a cloned underlying
            // file wrapped back into a handle is the same identity.
            #[cfg(not(unix))]
            root_handle: same_file::Handle::from_file(self.root_handle.as_file().try_clone()?)?,
        })
    }
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
        Errno::LOOP | Errno::NOTDIR => PluginError::NamespaceChanged(format!(
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
    /// Walk the directories of `relative` down from the retained root; see
    /// [`open_parent_below`].
    fn open_payload_parent(
        &self,
        on_error: impl Fn(rustix::io::Errno, &Path) -> PluginError,
    ) -> Result<(std::os::fd::OwnedFd, &std::ffi::OsStr), PluginError> {
        use std::os::fd::AsFd;

        open_parent_below(self.root_dir.as_fd(), &self.relative, on_error)
    }
}

/// Walk the directories of `relative` down from `root`, opening each relative
/// to the one before without following a symlink, and return the payload's
/// parent directory and file name. `on_error` maps a failed open, given the
/// part of `relative` walked so far.
#[cfg(unix)]
fn open_parent_below<'a>(
    root: std::os::fd::BorrowedFd<'_>,
    relative: &'a Path,
    on_error: impl Fn(rustix::io::Errno, &Path) -> PluginError,
) -> Result<(std::os::fd::OwnedFd, &'a std::ffi::OsStr), PluginError> {
    use rustix::fs::{Mode, OFlags};

    let leaf = relative.file_name().ok_or_else(|| {
        PluginError::InvalidManifest(format!(
            "wasm_path must name a file (got {})",
            relative.display()
        ))
    })?;
    let mut dir = root.try_clone_to_owned()?;
    let mut walked = PathBuf::new();
    for segment in relative.parent().into_iter().flat_map(Path::iter) {
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

/// Open the payload `leaf` in `parent` without following a symlink.
/// Non-blocking, so a FIFO or device fails the regular-file check instead of
/// blocking here; `NOCTTY` keeps a terminal from becoming the controlling one.
#[cfg(unix)]
fn open_payload_leaf(
    parent: &std::os::fd::OwnedFd,
    leaf: &std::ffi::OsStr,
) -> rustix::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::openat(
        parent,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
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

    read_component_bytes(file, opened_metadata.len())
}

/// Shared bounded payload read: both ambient source admission and claimed
/// directory admission enforce the same size policy before and during reading.
fn read_component_bytes(file: impl Read, len: u64) -> Result<Vec<u8>, PluginError> {
    if len > MAX_COMPONENT_BYTES {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload exceeds the {MAX_COMPONENT_BYTES}-byte admission limit"
        )));
    }
    // Bound the read, not just the length check above: the payload can grow
    // between that check and this read. Taking one byte past the limit makes an
    // oversized payload detectable without retaining more than that.
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MAX_COMPONENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_COMPONENT_BYTES {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload grew past the {MAX_COMPONENT_BYTES}-byte admission limit while being read"
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
    use rustix::fs::AtFlags;
    use rustix::io::Errno;

    // A symlink or non-directory where confinement found a directory fails the
    // no-follow directory open with ENOTDIR (ELOOP on older Linux kernels); a
    // symlink in place of the payload fails its no-follow open with ELOOP.
    let substituted = |errno: Errno| match errno {
        Errno::LOOP | Errno::NOTDIR => PluginError::NamespaceChanged(format!(
            "WASM payload path changed after confinement check: {}",
            path.display()
        )),
        other => PluginError::Io(other.into()),
    };

    let (parent, leaf) = confined.open_payload_parent(|errno, _walked| substituted(errno))?;
    #[cfg(test)]
    payload_step(PayloadStep::BeforeLeafOpen);
    let payload = open_payload_leaf(&parent, leaf).map_err(substituted)?;
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
        return Err(PluginError::NamespaceChanged(format!(
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
        PluginError::NamespaceChanged(format!(
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
        return Err(PluginError::NamespaceChanged(format!(
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

/// A point in discovery a test can intervene at.
#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiscoveryStep {
    /// The manifest decided admission; the component is admitted next.
    BeforeComponentAdmission,
}

/// What a test runs at each [`DiscoveryStep`].
#[cfg(all(test, unix))]
type DiscoveryStepHook = Box<dyn FnMut(DiscoveryStep)>;

#[cfg(all(test, unix))]
thread_local! {
    /// Called at every [`DiscoveryStep`] reached on this thread, when set.
    static DISCOVERY_STEP_HOOK: std::cell::RefCell<Option<DiscoveryStepHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
fn discovery_step(step: DiscoveryStep) {
    DISCOVERY_STEP_HOOK.with(|hook| {
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
    let dir = Dir::open_ambient_dir(plugin_dir, cap_std::ambient_authority())?;
    validate_manifest_shape_in(manifest, &dir, plugin_dir)
}

fn validate_manifest_shape_in(
    manifest: &PluginManifest,
    dir: &Dir,
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
        validate_skill_bundle(&manifest.name, dir, plugin_dir)?;
    }

    Ok(())
}

/// Validate a skill bundle: `<plugin_dir>/skills/` must exist, contain at least
/// one subdirectory, and each subdirectory must hold a `SKILL.md` whose YAML
/// frontmatter declares the agentskills.io-required `name` and `description`.
fn validate_skill_bundle(
    plugin_name: &str,
    dir: &Dir,
    plugin_dir: &Path,
) -> Result<(), PluginError> {
    let skills_dir = plugin_dir.join(SKILLS_SUBDIR);
    // Like a package root, the skills root is an admission boundary: a
    // symlink here would let the bundle validated and later copied live
    // outside the package.
    let skills_metadata = match dir.symlink_metadata(SKILLS_SUBDIR) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if skills_metadata
        .as_ref()
        .is_some_and(|m| m.file_type().is_symlink())
    {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}' has a symlinked `skills/` directory at {}; package the skills in place",
            plugin_name,
            skills_dir.display()
        )));
    }
    if !skills_metadata.is_some_and(|metadata| metadata.is_dir()) {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}' is missing `skills/` directory at {}",
            plugin_name,
            skills_dir.display()
        )));
    }

    let mut found_any = false;
    for entry in dir.read_dir(SKILLS_SUBDIR)? {
        let entry = entry?;
        let relative = Path::new(SKILLS_SUBDIR).join(entry.file_name());
        let path = plugin_dir.join(&relative);
        if !entry.file_type()?.is_dir() {
            continue;
        }
        found_any = true;
        let skill_md = path.join("SKILL.md");
        // Missing or wrong-type content is structural; an operational lookup
        // failure says nothing about the package and must not authorize recovery.
        let skill_metadata = match dir.metadata(relative.join("SKILL.md")) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if !skill_metadata.is_some_and(|metadata| metadata.is_file()) {
            return Err(PluginError::InvalidManifest(format!(
                "skill plugin '{}' subdirectory '{}' is missing SKILL.md",
                plugin_name,
                path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            )));
        }
        validate_skill_md_frontmatter(
            plugin_name,
            &skill_md,
            dir.read_to_string(relative.join("SKILL.md"))?,
        )?;
    }

    if !found_any {
        return Err(PluginError::InvalidManifest(format!(
            "skill plugin '{}' has empty `skills/` directory",
            plugin_name
        )));
    }

    Ok(())
}

fn validate_skill_md_frontmatter(
    plugin_name: &str,
    skill_md: &Path,
    content: String,
) -> Result<(), PluginError> {
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

/// Shared prefix for legacy PID-only stages and new leased generations.
/// Dot-prefixed, so discovery never loads either kind as a package.
fn staging_prefix(package: &str) -> String {
    format!(".{package}.installing-")
}

/// A directory's identity, so two spellings of one directory compare equal.
/// Read from its metadata without opening it.
#[cfg(unix)]
fn directory_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

/// A directory's identity, so two spellings of one directory compare equal.
#[cfg(not(unix))]
fn directory_identity(path: &Path) -> std::io::Result<same_file::Handle> {
    same_file::Handle::from_path(path)
}

/// Whether an admission failure comes from the package directory's own
/// contents: a truncated or unparsable manifest, a missing component, a
/// component that does not match its declared digest, or anything else this
/// host cannot accept as written, such as a component over the admission size
/// limit, a `config_schema` it cannot compile, or an incomplete skill bundle.
/// For a directory that holds a `manifest.toml`, this is the only verdict
/// under which `remove` deletes a directory the host never loaded, whether an
/// interrupted install left it or it holds a package this host could never
/// load.
///
/// Every other outcome keeps the directory. A trust-policy failure describes a
/// package this host declines to load under its signature policy; admission
/// checks the signature before the component, so under `strict` that includes
/// an unsigned interrupted install.
/// `AlreadyLoaded` means the manifest names a package the host has loaded. An
/// I/O error says nothing about the package. The match has no wildcard arm, so
/// a new error variant does not compile until it is classified here.
fn is_structural_admission_failure(error: &PluginError) -> bool {
    match error {
        PluginError::NotFound(_)
        | PluginError::InvalidManifest(_)
        | PluginError::TomlParse(_)
        | PluginError::PayloadDigestInvalid(_)
        | PluginError::PayloadDigestMismatch { .. } => true,
        // A manifest cut off inside a multi-byte character is not UTF-8.
        PluginError::Io(error) => error.kind() == std::io::ErrorKind::InvalidData,
        PluginError::UnsignedPlugin(_)
        | PluginError::UntrustedPublisher { .. }
        | PluginError::SignatureInvalid(_)
        | PluginError::PayloadDigestRequired(_)
        | PluginError::AlreadyLoaded(_)
        | PluginError::UnadmittedPackage { .. }
        | PluginError::NamespaceChanged(_)
        | PluginError::RecoveryRetained { .. }
        | PluginError::ReplacementInterrupted { .. }
        | PluginError::MoveUnsupported(_)
        | PluginError::MoveUnsupportedCleanup { .. }
        | PluginError::SourceInStage { .. }
        | PluginError::InvalidConfig(_)
        | PluginError::InvalidInstanceId(_)
        | PluginError::InvalidEndpoint(_)
        | PluginError::LoadFailed(_)
        | PluginError::ExecutionFailed(_)
        | PluginError::PermissionDenied { .. }
        | PluginError::UnsupportedCapability(_) => false,
    }
}

/// Why `dir`, claimed as `name`, holds something no install could have
/// written, if it does. Every installer writes a package under its manifest's
/// name: `manifest.toml` first, and after it only the component at `wasm_path`,
/// a regular file with the directories on its way, and, for a skill plugin, a
/// `skills/` tree of directories and regular files. So when the manifest does
/// not parse, nothing else was written yet, and otherwise only those entries
/// can be an interrupted install's. Anything else, such as a plugin's source
/// tree next to a manifest whose component is not built, is not recovery's to
/// delete.
fn beyond_install_footprint(dir: &Dir, name: &str) -> Result<Option<String>, PluginError> {
    let manifest = dir
        .read_to_string("manifest.toml")
        .ok()
        .and_then(|text| toml::from_str::<PluginManifest>(&text).ok());
    if let Some(manifest) = &manifest
        && manifest.name != name
    {
        return Ok(Some(format!(
            "its manifest names '{}', and an install writes each package under its own name",
            manifest.name
        )));
    }
    let parsed = manifest.is_some();
    let skill_bundle = manifest
        .as_ref()
        .is_some_and(|manifest| manifest.capabilities.contains(&PluginCapability::Skill));
    let component: Vec<std::ffi::OsString> = manifest
        .as_ref()
        .and_then(|manifest| manifest.wasm_path.as_deref())
        .filter(|path| validate_manifest_subpath("wasm_path", "", path).is_ok())
        .map(|path| {
            Path::new(path)
                .components()
                .filter_map(|component| match component {
                    Component::Normal(segment) => Some(segment.to_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let foreign = |extra: PathBuf| {
        if parsed {
            format!(
                "it holds files an install never writes, such as '{}'",
                extra.display()
            )
        } else {
            format!(
                "its manifest.toml does not parse, so nothing beside it can be an interrupted install's, yet it holds '{}'",
                extra.display()
            )
        }
    };
    for entry in dir.entries()? {
        let entry = entry?;
        let entry_name = entry.file_name();
        if entry_name == "manifest.toml" {
            continue;
        }
        if skill_bundle && entry_name == SKILLS_SUBDIR && entry_type(dir, &entry)?.is_dir() {
            let skills = dir.open_dir(SKILLS_SUBDIR)?;
            if let Some(extra) = beyond_copied_tree(&skills, Path::new(SKILLS_SUBDIR))? {
                return Ok(Some(foreign(extra)));
            }
            continue;
        }
        if let Some(extra) = beyond_component_path(dir, &entry, &component, Path::new(""))? {
            return Ok(Some(foreign(extra)));
        }
    }
    Ok(None)
}

/// The type of `entry` in `dir`, without following a symlink. A filesystem
/// that does not report entry types while listing gives an unknown type, so
/// that case asks the directory for the entry's metadata instead.
fn entry_type(dir: &Dir, entry: &cap_std::fs::DirEntry) -> std::io::Result<cap_std::fs::FileType> {
    let file_type = entry.file_type()?;
    if file_type.is_dir() || file_type.is_file() || file_type.is_symlink() {
        return Ok(file_type);
    }
    Ok(dir.symlink_metadata(entry.file_name())?.file_type())
}

/// The first entry below a copied `skills/` tree that the copy could not have
/// made: every installer copies only directories and regular files.
fn beyond_copied_tree(dir: &Dir, walked: &Path) -> Result<Option<PathBuf>, PluginError> {
    for entry in dir.entries()? {
        let entry = entry?;
        let here = walked.join(entry.file_name());
        let file_type = entry_type(dir, &entry)?;
        if file_type.is_dir() {
            if let Some(extra) = beyond_copied_tree(&dir.open_dir(entry.file_name())?, &here)? {
                return Ok(Some(extra));
            }
        } else if !file_type.is_file() {
            return Ok(Some(here));
        }
    }
    Ok(None)
}

/// Whether `entry` lies on the way to the component, which must be the next of
/// `component`'s remaining segments: a directory with only the rest of the way
/// below it, or, for the last segment, the component itself.
fn beyond_component_path(
    parent: &Dir,
    entry: &cap_std::fs::DirEntry,
    component: &[std::ffi::OsString],
    walked: &Path,
) -> Result<Option<PathBuf>, PluginError> {
    let entry_name = entry.file_name();
    let here = walked.join(&entry_name);
    let Some((next, rest)) = component.split_first() else {
        return Ok(Some(here));
    };
    if entry_name != *next {
        return Ok(Some(here));
    }
    let file_type = entry_type(parent, entry)?;
    if rest.is_empty() {
        // An installer writes the component as a regular file, never as a
        // directory, a symlink or a special file.
        return Ok((!file_type.is_file()).then_some(here));
    }
    if !file_type.is_dir() {
        return Ok(Some(here));
    }
    let child = parent.open_dir(&entry_name)?;
    for inner in child.entries()? {
        if let Some(extra) = beyond_component_path(&child, &inner?, rest, &here)? {
            return Ok(Some(extra));
        }
    }
    Ok(None)
}

/// The plugins root, opened the first time a package operation needs it. Only
/// install, update, remove, and listing what an interrupted update displaced
/// take the package lock, so a host that only discovers plugins never opens
/// the root. That matters on Windows, where an open root pins the plugins
/// directory and every ancestor against rename for as long as the host lives.
/// The cell is a field of its own so that holding the root never borrows the
/// rest of the host.
fn opened_root<'a>(
    cell: &'a std::sync::OnceLock<recovery::Root>,
    plugins_dir: &Path,
) -> Result<&'a recovery::Root, PluginError> {
    if let Some(root) = cell.get() {
        return Ok(root);
    }
    let root = recovery::Root::open(plugins_dir)?;
    Ok(cell.get_or_init(|| root))
}

/// Record `current`, what discovery now admits under `name`, in place of this
/// host's earlier view of that one package, leaving every other package as
/// the host last admitted it. A free function over the loaded map, so it can
/// run while the plugins root, a field of its own, is held.
fn set_loaded(
    loaded: &mut HashMap<String, LoadedPlugin>,
    name: &str,
    current: Option<LoadedPlugin>,
) {
    match current {
        Some(plugin) => {
            loaded.insert(name.to_owned(), plugin);
        }
        None => {
            loaded.remove(name);
        }
    }
}

/// Finish `tx` once its package is gone or back at its name. Only the
/// transaction's entry is left then, so a failure names that entry as
/// [`PluginError::RecoveryRetained`] instead of failing the work already done
/// with a bare I/O error. A namespace change still refuses as one.
fn finish_naming_entry(
    plugins_dir: &Path,
    root: &recovery::Root,
    tx: recovery::Transaction,
) -> Result<(), PluginError> {
    let entry = plugins_dir.join(&tx.entry).display().to_string();
    tx.finish(root).map_err(|error| match error {
        PluginError::NamespaceChanged(_) => error,
        error => PluginError::RecoveryRetained {
            path: entry,
            reason: format!(
                "only the transaction entry is left, and it could not be removed: {error}"
            ),
        },
    })
}

/// The directory a manifest path names its package in: its parent, or the
/// current directory for a bare file name, whose parent is the empty path that
/// no directory handle opens.
fn manifest_source_dir(manifest_path: &Path) -> Result<PathBuf, PluginError> {
    let parent = manifest_path
        .parent()
        .ok_or_else(|| PluginError::InvalidManifest("no parent directory".into()))?;
    Ok(if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    })
}

/// The identity of a claimed package directory, through a handle of its own.
fn claimed_identity(claimed: &Dir) -> std::io::Result<same_file::Handle> {
    #[cfg(test)]
    if identity_fault::take() {
        return Err(std::io::Error::other("injected identity fault"));
    }
    same_file::Handle::from_file(claimed.try_clone()?.into_std_file())
}

/// Test-only fault for [`claimed_identity`], as a process out of file
/// descriptors would meet it. The slot is thread-local, like [`write_fault`]'s,
/// so a case arms only its own remove.
#[cfg(test)]
mod identity_fault {
    thread_local! {
        static ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Make the next identity check on this thread fail. It fires once.
    pub(super) fn arm() {
        ARMED.with(|armed| armed.set(true));
    }

    pub(super) fn take() -> bool {
        ARMED.with(|armed| armed.replace(false))
    }
}

/// The actual failed cleanup operation determines the diagnostic location.
/// This carrier does not confer ownership or assert the original bytes remain.
struct UnpublishedStageFailure {
    path: String,
    error: PluginError,
}

/// Missing root/lock lookups mean the cleanup namespace disappeared, rather
/// than an ordinary cleanup I/O failure. Other root-check errors stay I/O.
fn check_cleanup_namespace(root: &recovery::Root) -> Result<(), PluginError> {
    root.check().map_err(|error| match error {
        PluginError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            PluginError::NamespaceChanged(
                "plugins root or package coordination lock is no longer present".into(),
            )
        }
        error => error,
    })
}

/// A detected namespace change takes priority, but another check failure must
/// not erase the actual package-open error that led to this diagnostic.
fn cleanup_open_error(open: std::io::Error, check: Result<(), PluginError>) -> PluginError {
    match check {
        Err(namespace @ PluginError::NamespaceChanged(_)) => namespace,
        _ => open.into(),
    }
}

/// `package` is the original staged capability on Unix. Only callers that had
/// to close it before publication pass None and reopen through the held entry.
fn cleanup_after_unsupported_publish(
    plugins_dir: &Path,
    root: &recovery::Root,
    tx: recovery::Transaction,
    package: Option<Dir>,
    unsupported: std::io::Error,
) -> PluginError {
    let package = match package {
        Some(package) => Ok(package),
        None => tx.dir.open_dir(recovery::PACKAGE),
    };
    let cleared = match package {
        Ok(package) => clear_unpublished_stage(plugins_dir, root, tx, package),
        Err(open) => {
            let path = if open.kind() == std::io::ErrorKind::NotFound {
                plugins_dir.join(&tx.entry).display().to_string()
            } else {
                root.retained_path(&tx)
            };
            Err(UnpublishedStageFailure {
                path,
                error: cleanup_open_error(open, check_cleanup_namespace(root)),
            })
        }
    };
    match cleared {
        Ok(()) => PluginError::MoveUnsupported(unsupported.to_string()),
        Err(UnpublishedStageFailure {
            error: namespace @ PluginError::NamespaceChanged(_),
            ..
        }) => namespace,
        Err(cleanup) => PluginError::MoveUnsupportedCleanup {
            path: cleanup.path,
            reason: format!(
                "{unsupported}; unpublished transaction cleanup failed: {}",
                cleanup.error
            ),
        },
    }
}

/// Clear only this unpublished transaction through its held capabilities.
/// Keep namespace refusals and the failed phase's location distinct, including
/// a finish failure after the package directory has already been removed.
fn clear_unpublished_stage(
    plugins_dir: &Path,
    root: &recovery::Root,
    tx: recovery::Transaction,
    package: Dir,
) -> Result<(), UnpublishedStageFailure> {
    let package_path = root.retained_path(&tx);
    let entry_path = plugins_dir.join(&tx.entry).display().to_string();
    let package_failure = |error| UnpublishedStageFailure {
        path: package_path.clone(),
        error,
    };
    #[cfg(test)]
    recovery::pause("install-cleanup-before-clear");
    check_cleanup_namespace(root).map_err(package_failure)?;
    #[cfg(all(
        test,
        any(target_os = "linux", target_os = "android", target_vendor = "apple")
    ))]
    cleanup_fault::before_clear(&package).map_err(|error| package_failure(error.into()))?;
    recovery::clear_owned(&package).map_err(package_failure)?;
    drop(package);
    #[cfg(test)]
    recovery::pause("install-cleanup-before-package-unlink");
    check_cleanup_namespace(root).map_err(package_failure)?;
    tx.dir
        .remove_dir(recovery::PACKAGE)
        .map_err(|error| package_failure(error.into()))?;
    #[cfg(test)]
    recovery::pause("install-cleanup-before-finish");
    check_cleanup_namespace(root).map_err(|error| UnpublishedStageFailure {
        path: entry_path.clone(),
        error,
    })?;
    tx.finish(root).map_err(|error| UnpublishedStageFailure {
        path: entry_path,
        error,
    })
}

/// A test-only cleanup I/O fault after one actual staged file was deleted.
/// The remaining files are real, so the caller must report partial cleanup.
/// This exercises propagation beside `clear_owned`, not its internal `?`.
#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_vendor = "apple")
))]
mod cleanup_fault {
    thread_local! {
        static ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(super) struct Armed;

    impl Drop for Armed {
        fn drop(&mut self) {
            ARMED.with(|armed| armed.set(false));
        }
    }

    pub(super) fn arm() -> Armed {
        ARMED.with(|armed| armed.set(true));
        Armed
    }

    pub(super) fn before_clear(package: &cap_std::fs::Dir) -> std::io::Result<()> {
        if ARMED.with(|armed| armed.replace(false)) {
            package.remove_file("manifest.toml")?;
            return Err(std::io::Error::other("injected partial cleanup fault"));
        }
        Ok(())
    }
}

/// Test-only fault for [`PluginHost::sweep_stage`]: the first leased stage
/// the sweep reaches on this thread fails before its lease is taken, so a case
/// can tell a sweep that goes on past it from one that stops there. It fires
/// once.
#[cfg(test)]
mod sweep_fault {
    thread_local! {
        static ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(super) struct Armed;

    impl Drop for Armed {
        fn drop(&mut self) {
            ARMED.with(|armed| armed.set(false));
        }
    }

    pub(super) fn arm() -> Armed {
        ARMED.with(|armed| armed.set(true));
        Armed
    }

    pub(super) fn take() -> std::io::Result<()> {
        if ARMED.with(|armed| armed.replace(false)) {
            return Err(std::io::Error::other("injected stage sweep fault"));
        }
        Ok(())
    }
}

/// Why recovery keeps a directory it claimed, worded for the operator.
fn kept(name: &str, reason: impl Into<String>) -> PluginError {
    PluginError::UnadmittedPackage {
        name: name.into(),
        reason: reason.into(),
    }
}

/// Write an admitted package into `dir`: the exact manifest and component
/// bytes admission read, plus the `skills/` subtree of a skill-capable package.
fn write_package(
    dir: &Dir,
    manifest: &PluginManifest,
    manifest_toml: &str,
    source_dir: &Path,
    component: Option<&AdmittedComponent>,
) -> Result<(), PluginError> {
    // Persist the exact manifest and payload generations admitted above.
    #[cfg(test)]
    write_fault::inject(
        write_fault::Step::Manifest,
        dir,
        Path::new("manifest.toml"),
        manifest_toml.as_bytes(),
    )?;
    dir.write("manifest.toml", manifest_toml.as_bytes())?;

    // Copy skills/ subtree for skill-capable plugins.
    if manifest.capabilities.contains(&PluginCapability::Skill) {
        let src_skills = source_dir.join(SKILLS_SUBDIR);
        if src_skills.is_dir() {
            copy_skills_into(&src_skills, dir, Path::new(SKILLS_SUBDIR))?;
        }
    }

    // The component goes last: a `wasm_path` under `skills/` must hold the
    // admitted bytes, not whatever the source holds by the time it is copied.
    if let (Some(rel), Some(component)) = (manifest.wasm_path.as_deref(), component) {
        let dest = Path::new(rel);
        if let Some(parent) = dest.parent() {
            dir.create_dir_all(parent)?;
        }
        #[cfg(test)]
        write_fault::inject(write_fault::Step::Payload, dir, dest, component.bytes())?;
        dir.write(dest, component.bytes())?;
    }
    Ok(())
}

fn copy_skills_into(src: &Path, dir: &Dir, dest: &Path) -> Result<(), PluginError> {
    dir.create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_skills_into(&entry.path(), dir, &dest.join(entry.file_name()))?;
        } else if entry.file_type()?.is_file() {
            let source = std::fs::File::open(entry.path())?;
            #[cfg(unix)]
            let executable =
                std::os::unix::fs::PermissionsExt::mode(&source.metadata()?.permissions()) & 0o111;
            let mut target = dir.create(dest.join(entry.file_name()))?;
            #[cfg(test)]
            let source = write_fault::skill_reader(source, &entry.file_name());
            let mut source = source;
            copy_skill_data(&mut source, &mut target)?;
            #[cfg(unix)]
            {
                use cap_std::fs::PermissionsExt;

                // Keep the staging file's creation policy for read/write and
                // special bits; only transfer the source's executable intent.
                let mut permissions = target.metadata()?.permissions();
                let mode = (permissions.mode() & !0o111) | executable;
                if mode != permissions.mode() {
                    permissions.set_mode(mode);
                    target.set_permissions(permissions)?;
                }
            }
        }
    }
    Ok(())
}

/// Copy progressively with a fixed memory bound, including short writes and
/// interrupted reads. A failure leaves only an owned staging file; the caller
/// propagates it through the existing install cleanup before publication.
fn copy_skill_data(
    source: &mut impl std::io::Read,
    target: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let mut buffer = [0u8; 8 * 1024];
    loop {
        match source.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => target.write_all(&buffer[..read])?,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
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
        // Discovery skips linked child packages; leave those links at source.
        // The root itself may still be a supported legacy-directory alias.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) || !src.join("manifest.toml").exists()
        {
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

/// Test-only write fault injected into [`write_package`].
///
/// Production compiles this away entirely. Under test it cuts the armed write
/// off part-way, as a full disk or a killed process would, which is what lets
/// a case prove that a failure at that exact step leaves nothing behind. The
/// slot is thread-local, so a case running in parallel with others arms only
/// its own install.
#[cfg(test)]
mod write_fault {
    /// Package I/O a test can make fail.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Step {
        Manifest,
        Payload,
        SkillRead,
    }

    thread_local! {
        static ARMED: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
    }

    /// Clears the slot on drop, so a fault never leaks into a later case
    /// that runs on the same thread.
    pub(super) struct Armed;

    impl Drop for Armed {
        fn drop(&mut self) {
            ARMED.with(|armed| armed.set(None));
        }
    }

    /// Make the next `step` write on this thread fail. It fires once.
    pub(super) fn arm(step: Step) -> Armed {
        ARMED.with(|armed| armed.set(Some(step)));
        Armed
    }

    /// When `step` is armed, write the first half of `bytes` to `path` and
    /// fail, leaving the truncated file an interrupted write would.
    pub(super) fn inject(
        step: Step,
        dir: &cap_std::fs::Dir,
        path: &std::path::Path,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        if ARMED.with(|armed| armed.get()) != Some(step) {
            return Ok(());
        }
        ARMED.with(|armed| armed.set(None));
        dir.write(path, &bytes[..bytes.len() / 2])?;
        Err(std::io::Error::other(format!(
            "injected {step:?} write fault"
        )))
    }

    /// Fail partway through the synthetic fail.bin asset, leaving other skill
    /// files and parallel installs unaffected. The source file is still real.
    pub(super) fn skill_reader(file: std::fs::File, name: &std::ffi::OsStr) -> SkillReader {
        let armed = ARMED.with(|slot| {
            if name == "fail.bin" && slot.get() == Some(Step::SkillRead) {
                slot.set(None);
                true
            } else {
                false
            }
        });
        SkillReader {
            file,
            remaining: armed.then_some(4096),
        }
    }

    pub(super) struct SkillReader {
        file: std::fs::File,
        remaining: Option<usize>,
    }

    impl std::io::Read for SkillReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some(remaining) = self.remaining else {
                return std::io::Read::read(&mut self.file, buffer);
            };
            if remaining == 0 {
                return Err(std::io::Error::other("injected skill asset read fault"));
            }
            let take = buffer.len().min(remaining);
            let read = std::io::Read::read(&mut self.file, &mut buffer[..take])?;
            self.remaining = Some(remaining - read);
            Ok(read)
        }
    }
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

    /// Install a synthetic skill, preserving executable intent without
    /// changing the staging policy for ordinary assets or copying special bits.
    #[cfg(unix)]
    #[test]
    fn skill_install_preserves_execute_bits_and_non_executable_assets() {
        use std::os::unix::fs::PermissionsExt;

        let source = tempdir().unwrap();
        write_skill_bundle_plugin(source.path(), "copy-modes", &["fixture"]);
        let package = source.path().join("copy-modes");
        let skill = package.join("skills/fixture");
        let script = b"#!/bin/sh\nprintf 'isolated copy fixture\\n'\n";
        for (name, mode) in [
            ("helper.sh", 0o755),
            ("private.sh", 0o700),
            ("special.sh", 0o4755),
        ] {
            std::fs::write(skill.join(name), script).unwrap();
            std::fs::set_permissions(skill.join(name), std::fs::Permissions::from_mode(mode))
                .unwrap();
        }
        std::fs::write(skill.join("notes.txt"), b"ordinary asset").unwrap();
        std::fs::set_permissions(
            skill.join("notes.txt"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        host.install(package.to_str().unwrap()).unwrap();
        let installed = plugins.path().join("copy-modes/skills/fixture");
        let ordinary_mode = std::fs::metadata(installed.join("SKILL.md"))
            .unwrap()
            .permissions()
            .mode();

        for (name, execute) in [
            ("helper.sh", 0o111),
            ("private.sh", 0o100),
            ("special.sh", 0o111),
        ] {
            assert_eq!(std::fs::read(installed.join(name)).unwrap(), script);
            let mode = std::fs::metadata(installed.join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, execute, "{name}");
            assert_eq!(mode & 0o7000, 0, "special bits must not transfer: {name}");
            assert_eq!(mode & 0o666, ordinary_mode & 0o666, "{name}");
        }
        assert_eq!(
            std::fs::read(installed.join("notes.txt")).unwrap(),
            b"ordinary asset"
        );
        assert_eq!(
            std::fs::metadata(installed.join("notes.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        // Only this literal test fixture is invoked; production never executes
        // the copied package. Mode assertions run first for a causal failure.
        let output = std::process::Command::new(installed.join("helper.sh"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"isolated copy fixture\n");
    }

    #[test]
    fn skill_copy_streams_large_input_with_short_writes_and_interruptions() {
        use std::cell::Cell;
        use std::rc::Rc;

        // The producer permits only one unwritten chunk. Reading a whole
        // asset first fails, while a progressive copy needs no asset-sized Vec.
        struct Producer {
            pending: Rc<Cell<usize>>,
            offset: usize,
            total: usize,
            largest_request: usize,
            interrupt: bool,
        }
        impl std::io::Read for Producer {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.largest_request = self.largest_request.max(buffer.len());
                if self.interrupt {
                    self.interrupt = false;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                if self.pending.get() != 0 {
                    return Err(std::io::Error::other("asset was read ahead of its output"));
                }
                let read = buffer.len().min(3073).min(self.total - self.offset);
                for (i, byte) in buffer[..read].iter_mut().enumerate() {
                    *byte = ((self.offset + i) % 251) as u8;
                }
                self.offset += read;
                self.pending.set(read);
                Ok(read)
            }
        }
        struct Consumer {
            pending: Rc<Cell<usize>>,
            offset: usize,
            interrupt: bool,
        }
        impl std::io::Write for Consumer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.interrupt {
                    self.interrupt = false;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                let written = bytes.len().min(127);
                for (i, byte) in bytes[..written].iter().enumerate() {
                    assert_eq!(*byte, ((self.offset + i) % 251) as u8);
                }
                self.offset += written;
                self.pending.set(self.pending.get() - written);
                Ok(written)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let pending = Rc::new(Cell::new(0));
        let mut source = Producer {
            pending: pending.clone(),
            offset: 0,
            total: 32 * 1024 * 1024,
            largest_request: 0,
            interrupt: true,
        };
        let mut target = Consumer {
            pending,
            offset: 0,
            interrupt: true,
        };
        copy_skill_data(&mut source, &mut target).unwrap();
        assert_eq!(source.offset, source.total);
        assert_eq!(target.offset, source.total);
        assert!(
            source.largest_request <= 64 * 1024,
            "read buffer grew with asset size"
        );
        assert_eq!(target.pending.get(), 0);
    }

    #[test]
    fn skill_copy_preserves_written_prefix_and_propagates_read_failure() {
        struct PartialRead(bool);
        impl std::io::Read for PartialRead {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "asset became unreadable",
                    ));
                }
                self.0 = true;
                bytes[..6].copy_from_slice(b"prefix");
                Ok(6)
            }
        }
        let dir = tempdir().unwrap();
        let mut target = std::fs::File::create(dir.path().join("partial")).unwrap();
        let error = copy_skill_data(&mut PartialRead(false), &mut target).unwrap_err();
        assert_eq!(
            std::fs::read(dir.path().join("partial")).unwrap(),
            b"prefix"
        );
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "asset became unreadable");
    }

    #[test]
    fn skill_install_cleans_partial_asset_after_read_failure() {
        fn snapshot(root: &Path, dir: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if entry.file_type().unwrap().is_dir() {
                    entries.push((relative, None));
                    entries.extend(snapshot(root, &path));
                } else {
                    assert!(entry.file_type().unwrap().is_file());
                    entries.push((relative, Some(std::fs::read(&path).unwrap())));
                }
            }
            entries.sort();
            entries
        }

        let source = tempdir().unwrap();
        write_skill_bundle_plugin(source.path(), "copy-error", &["fixture"]);
        let package = source.path().join("copy-error");
        std::fs::write(
            package.join("skills/fixture/fail.bin"),
            vec![0x5a; 32 * 1024],
        )
        .unwrap();
        let before = snapshot(&package, &package);
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        {
            let _fault = write_fault::arm(write_fault::Step::SkillRead);
            let error = host.install(package.to_str().unwrap()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("injected skill asset read fault"),
                "{error:?}"
            );
        }
        assert!(!plugins.path().join("copy-error").exists());
        assert!(host.get_plugin("copy-error").is_none());
        assert_eq!(dir_entries(plugins.path()), [".zeroclaw-package-lock-v1"]);
        assert_eq!(snapshot(&package, &package), before);
        // Clearing the fault permits the real package path to complete.
        host.install(package.to_str().unwrap()).unwrap();
        let installed = plugins.path().join("copy-error");
        assert_eq!(snapshot(&installed, &installed), before);
    }

    #[cfg(unix)]
    #[test]
    fn skill_copy_skips_source_links_and_refuses_destination_escape() {
        let source = tempdir().unwrap();
        let target = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(source.path().join("asset"), b"copied asset").unwrap();
        std::fs::write(outside.path().join("keep"), b"outside bytes").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("keep"),
            source.path().join("linked-file"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), source.path().join("linked-dir")).unwrap();
        let dir = Dir::open_ambient_dir(target.path(), cap_std::ambient_authority()).unwrap();
        copy_skills_into(source.path(), &dir, Path::new("skills")).unwrap();
        assert_eq!(
            std::fs::read(target.path().join("skills/asset")).unwrap(),
            b"copied asset"
        );
        assert!(!target.path().join("skills/linked-file").exists());
        assert!(!target.path().join("skills/linked-dir").exists());

        std::os::unix::fs::symlink(outside.path(), target.path().join("redirect")).unwrap();
        assert!(copy_skills_into(source.path(), &dir, Path::new("redirect")).is_err());
        assert_eq!(
            std::fs::read(outside.path().join("keep")).unwrap(),
            b"outside bytes"
        );
        assert!(!outside.path().join("asset").exists());
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

    pub(super) fn write_tool_source(dir: &Path, name: &str, wasm: &[u8]) {
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
    /// instead of being refused by a half-written package. The
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
        assert_eq!(
            left,
            [std::ffi::OsString::from(".zeroclaw-package-lock-v1")],
            "only the persistent coordination file may remain"
        );
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

    /// Leave an abandoned `kind` transaction of `name` holding a package, as a
    /// stopped process leaves one, and return its entry and package path.
    fn abandon_transaction(host: &PluginHost, name: &str, kind: &str) -> (String, PathBuf) {
        let tx = host.root().unwrap().transaction(name, kind).unwrap();
        tx.dir.create_dir(recovery::PACKAGE).unwrap();
        let package = host.plugins_dir().join(&tx.entry).join(recovery::PACKAGE);
        write_tool_source(&package, name, b"\0asm interrupted");
        // Dropping the transaction releases its lease, as a stopped process's
        // lease is released when it ends.
        (tx.entry.clone(), package)
    }

    /// Installing a package clears the `installing` stages abandoned for its
    /// name, once their leases are free, and nothing else: another package's
    /// stage, a legacy PID-named stage, whose owner cannot be proved, and the
    /// name's `replacing` and `recovering` transactions, which can hold the
    /// only copy of a package, are kept as they are.
    #[test]
    fn install_clears_abandoned_stages_of_its_name_only() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        abandon_transaction(&host, "weather", "installing");
        let kept: Vec<_> = [
            ("other", "installing"),
            ("weather", "replacing"),
            ("weather", "recovering"),
        ]
        .into_iter()
        .map(|(name, kind)| {
            let (entry, package) = abandon_transaction(&host, name, kind);
            let bytes = package_bytes(&package);
            (entry, package, bytes)
        })
        .collect();
        let legacy = plugins.path().join(".weather.installing-4242");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("sentinel"), b"legacy stage").unwrap();
        let legacy_bytes = package_bytes(&legacy);

        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        let mut expected: Vec<String> = kept.iter().map(|(entry, ..)| entry.clone()).collect();
        expected.push(".weather.installing-4242".to_string());
        expected.sort();
        assert_eq!(hidden_entries(plugins.path()), expected);
        for (entry, package, bytes) in &kept {
            assert_eq!(&package_bytes(package), bytes, "{entry}");
        }
        assert_eq!(package_bytes(&legacy), legacy_bytes);
        assert_eq!(
            std::fs::read(plugins.path().join("weather/plugin.wasm")).unwrap(),
            b"\0asm v1"
        );
    }

    /// An install refused because something holds the name changes nothing,
    /// not even an abandoned stage of that name.
    #[test]
    fn a_refused_install_leaves_abandoned_stages_alone() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let (entry, package) = abandon_transaction(&host, "weather", "installing");
        let bytes = package_bytes(&package);
        std::fs::write(plugins.path().join("weather"), b"not a package").unwrap();

        let source = tempdir().unwrap();
        write_tool_source(source.path(), "weather", b"\0asm v1");
        let err = host
            .install(source.path().to_str().unwrap())
            .expect_err("a file holds the name");

        assert!(
            matches!(err, PluginError::UnadmittedPackage { .. }),
            "{err}"
        );
        assert_eq!(hidden_entries(plugins.path()), [entry]);
        assert_eq!(package_bytes(&package), bytes);
    }

    /// A stage the sweep cannot clear does not fail the install: it is left
    /// as it is, and the package is installed.
    #[cfg(unix)]
    #[test]
    fn install_goes_on_past_a_stage_it_cannot_clear() {
        if !permissions_can_block_a_delete() {
            return;
        }
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let (entry, package) = abandon_transaction(&host, "weather", "installing");
        lock_a_file_inside(&package);

        let source = tempdir().unwrap();
        write_tool_source(source.path(), "weather", b"\0asm v1");
        let installed = host.install(source.path().to_str().unwrap());
        let left = dir_entries(&package);
        unlock(&package);

        assert_eq!(installed.unwrap(), "weather");
        assert_eq!(hidden_entries(plugins.path()), [entry]);
        assert!(left.contains(&"locked".to_string()), "{left:?}");
        assert_eq!(
            std::fs::read(plugins.path().join("weather/plugin.wasm")).unwrap(),
            b"\0asm v1"
        );
    }

    /// A stage the sweep cannot clear does not stop it: the stages after it
    /// are still cleared. The fault fails whichever stage the sweep reaches
    /// first, so exactly one of the two is left whatever order they are
    /// listed in.
    #[test]
    fn install_clears_the_stages_after_one_it_cannot_clear() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let first = abandon_transaction(&host, "weather", "installing").0;
        let second = abandon_transaction(&host, "weather", "installing").0;

        let fault = sweep_fault::arm();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        drop(fault);

        let left = hidden_entries(plugins.path());
        assert!(
            left == [first.clone()] || left == [second.clone()],
            "exactly one stage is left: {left:?}"
        );
    }

    /// A stage another host in this process is still building is never
    /// cleared: its lease is held through an open of its own, which the
    /// sweep's open cannot take. The abandoned stage next to it shows the
    /// sweep ran.
    #[test]
    fn install_keeps_a_stage_another_host_in_this_process_holds() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let other = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        abandon_transaction(&host, "weather", "installing");
        let live = other
            .root()
            .unwrap()
            .transaction("weather", "installing")
            .unwrap();
        live.dir.create_dir(recovery::PACKAGE).unwrap();
        live.dir.write("package/bytes", b"live stage").unwrap();

        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        assert_eq!(hidden_entries(plugins.path()), [live.entry.as_str()]);
        assert_eq!(live.dir.read("package/bytes").unwrap(), b"live stage");
        drop(live);
    }

    /// Leave an abandoned `installing` stage of `name` holding a whole skill
    /// package, as an install stopped before publishing leaves one, and
    /// return its entry and package path.
    fn abandon_skill_stage(host: &PluginHost, name: &str) -> (String, PathBuf) {
        let source = tempdir().unwrap();
        write_skill_bundle_plugin(source.path(), name, &["alpha"]);
        let tx = host
            .root()
            .unwrap()
            .transaction(name, "installing")
            .unwrap();
        let package = host.plugins_dir().join(&tx.entry).join(recovery::PACKAGE);
        copy_dir_recursive(&source.path().join(name), &package).unwrap();
        (tx.entry.clone(), package)
    }

    /// A failed publication keeps the staged package and reports its path.
    /// Installing a skill package from that path refuses before the sweep
    /// clears anything, so the stage keeps the `skills/` tree admission did
    /// not retain, and so does another abandoned stage of the name. A copy
    /// taken out of the stage installs, and that install clears both stages.
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn an_install_from_the_stage_a_failed_install_kept_refuses_and_keeps_it() {
        struct Reset(Option<rustix::io::Errno>);
        impl Drop for Reset {
            fn drop(&mut self) {
                recovery::FORCE_RENAME_ERROR.with(|error| error.set(self.0));
            }
        }
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let source = tempdir().unwrap();
        write_skill_bundle_plugin(source.path(), "notes", &["alpha"]);
        let reset = Reset(
            recovery::FORCE_RENAME_ERROR
                .with(|error| error.replace(Some(rustix::io::Errno::EXIST))),
        );
        let kept = match host.install(source.path().join("notes").to_str().unwrap()) {
            Err(PluginError::RecoveryRetained { path, .. }) => PathBuf::from(path),
            other => panic!("a failed publication keeps its stage: {other:?}"),
        };
        drop(reset);
        let stage = kept.parent().unwrap().to_path_buf();
        let staged = nested_package_bytes(&kept);
        assert!(
            staged
                .iter()
                .any(|(path, _)| path == Path::new("skills/alpha/SKILL.md")),
            "{staged:?}"
        );
        let (other, other_package) = abandon_transaction(&host, "notes", "installing");
        let other_bytes = package_bytes(&other_package);
        let mut stages = vec![
            stage.file_name().unwrap().to_str().unwrap().to_string(),
            other,
        ];
        stages.sort();

        let err = host
            .install(kept.to_str().unwrap())
            .expect_err("the source is inside a stage the sweep would clear");

        assert!(
            matches!(&err, PluginError::SourceInStage { path } if Path::new(path) == stage),
            "{err}"
        );
        assert_eq!(hidden_entries(plugins.path()), stages);
        assert_eq!(nested_package_bytes(&kept), staged);
        assert_eq!(package_bytes(&other_package), other_bytes);
        assert!(!plugins.path().join("notes").exists());

        let copy = tempdir().unwrap();
        copy_dir_recursive(&kept, copy.path()).unwrap();
        assert_eq!(
            host.install(copy.path().to_str().unwrap()).unwrap(),
            "notes"
        );
        assert!(hidden_entries(plugins.path()).is_empty());
        assert_eq!(nested_package_bytes(&plugins.path().join("notes")), staged);
    }

    /// The source is matched to its stage by identity, not spelling: a
    /// trailing separator, a `.`, a `..` through the plugins directory's own
    /// name, the manifest's path, a symlink to the staged package, and the
    /// stage reached through a symlinked plugins directory each name it, and
    /// each install refuses and leaves the stage as it was.
    #[test]
    fn an_install_refuses_a_source_in_its_stage_however_the_path_is_spelled() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let (entry, package) = abandon_skill_stage(&host, "notes");
        let staged = nested_package_bytes(&package);
        let stage = plugins.path().join(&entry);
        let mut refuses = |source: &Path| {
            let err = host
                .install(source.to_str().unwrap())
                .expect_err("the source is inside a stage the sweep would clear");
            assert!(
                matches!(&err, PluginError::SourceInStage { path } if Path::new(path) == stage),
                "{}: {err}",
                source.display()
            );
            assert_eq!(
                hidden_entries(plugins.path()),
                [entry.as_str()],
                "{}",
                source.display()
            );
            assert_eq!(
                nested_package_bytes(&package),
                staged,
                "{}",
                source.display()
            );
        };

        refuses(&PathBuf::from(format!(
            "{}{}",
            package.display(),
            std::path::MAIN_SEPARATOR
        )));
        refuses(
            &plugins
                .path()
                .join(".")
                .join(&entry)
                .join(recovery::PACKAGE),
        );
        refuses(
            &plugins
                .path()
                .join("..")
                .join(plugins.path().file_name().unwrap())
                .join(&entry)
                .join(recovery::PACKAGE),
        );
        refuses(&package.join("manifest.toml"));
        #[cfg(unix)]
        {
            let links = tempdir().unwrap();
            let linked_package = links.path().join("package");
            std::os::unix::fs::symlink(&package, &linked_package).unwrap();
            refuses(&linked_package);
            let linked_plugins = links.path().join("plugins");
            std::os::unix::fs::symlink(plugins.path(), &linked_plugins).unwrap();
            refuses(&linked_plugins.join(&entry).join(recovery::PACKAGE));
        }
        assert!(!plugins.path().join("notes").exists());
    }

    /// Sorted names of every entry in `dir`, hidden ones included.
    pub(super) fn dir_entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Every file of a flat package directory with its exact bytes.
    pub(super) fn package_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
        dir_entries(dir)
            .into_iter()
            .map(|name| {
                let bytes = std::fs::read(dir.join(&name)).unwrap();
                (name, bytes)
            })
            .collect()
    }

    /// Cut one write of an install off part-way, then check what is left.
    ///
    /// The plugins directory must list exactly what it held before, hidden
    /// entries included, so a leftover staging directory fails the check as
    /// surely as a final one would. The package already installed stays
    /// byte-identical, and the retry installs the exact source bytes.
    fn assert_a_write_fault_leaves_nothing_behind(step: write_fault::Step) {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let existing = tempdir().unwrap();
        write_tool_source(existing.path(), "other", b"\0asm other");
        host.install(existing.path().to_str().unwrap()).unwrap();
        let other_before = package_bytes(&plugins.path().join("other"));

        let source = tempdir().unwrap();
        write_tool_source(source.path(), "faulty", b"\0asm faulty");
        let armed = write_fault::arm(step);
        let err = host
            .install(source.path().to_str().unwrap())
            .expect_err("the injected write fault must fail the install");
        drop(armed);
        assert!(err.to_string().contains("injected"), "{err}");

        assert_eq!(
            dir_entries(plugins.path()),
            [".zeroclaw-package-lock-v1", "other"],
            "a {step:?} write fault left something behind"
        );
        assert!(host.get_plugin("faulty").is_none());
        assert_eq!(package_bytes(&plugins.path().join("other")), other_before);

        assert_eq!(
            host.install(source.path().to_str().unwrap()).unwrap(),
            "faulty"
        );
        assert_eq!(
            package_bytes(&plugins.path().join("faulty")),
            package_bytes(source.path()),
            "the retry installs the exact source bytes"
        );
        assert_eq!(package_bytes(&plugins.path().join("other")), other_before);
    }

    #[test]
    fn a_manifest_write_fault_leaves_nothing_behind_and_the_retry_succeeds() {
        assert_a_write_fault_leaves_nothing_behind(write_fault::Step::Manifest);
    }

    #[test]
    fn a_payload_write_fault_leaves_nothing_behind_and_the_retry_succeeds() {
        assert_a_write_fault_leaves_nothing_behind(write_fault::Step::Payload);
    }

    /// What an installer that wrote straight into the package directory left
    /// when it stopped: right after creating the directory, inside the
    /// manifest write (at a line, or inside a multi-byte character), between
    /// the manifest and the payload, and inside the payload write. Discovery
    /// skips each one. Install refuses it as unadmitted rather than
    /// `AlreadyLoaded` and leaves it and all staging alone; `remove` deletes it
    /// with the name's stale staging directories only; the retry installs.
    #[test]
    fn a_stranded_unadmitted_directory_is_reported_distinctly_and_recoverable() {
        let payload: &[u8] = b"\0asm complete";
        let manifest = format!(
            "name = \"stranded\"\nversion = \"0.1.0\"\ndescription = \"\u{dc}bersetzt\"\nwasm_path = \"plugin.wasm\"\nwasm_sha256 = \"{}\"\ncapabilities = [\"tool\"]\n",
            signature::sha256_hex(payload)
        );
        let manifest = manifest.as_bytes();
        let first_line = manifest.iter().position(|&byte| byte == b'\n').unwrap() + 1;
        let mid_character = manifest.iter().position(|&byte| byte >= 0x80).unwrap() + 1;
        /// A stranded shape: its label, then each file it holds and its bytes.
        type Shape<'a> = (&'a str, &'a [(&'a str, &'a [u8])]);
        let shapes: [Shape<'_>; 5] = [
            ("the directory alone", &[]),
            (
                "a manifest cut at a line",
                &[("manifest.toml", &manifest[..first_line])],
            ),
            (
                "a manifest cut inside a character",
                &[("manifest.toml", &manifest[..mid_character])],
            ),
            (
                "a manifest without its payload",
                &[("manifest.toml", manifest)],
            ),
            (
                "a payload cut short",
                &[
                    ("manifest.toml", manifest),
                    ("plugin.wasm", &payload[..payload.len() / 2]),
                ],
            ),
        ];

        let source = tempdir().unwrap();
        std::fs::write(source.path().join("manifest.toml"), manifest).unwrap();
        std::fs::write(source.path().join("plugin.wasm"), payload).unwrap();

        for (shape, files) in shapes {
            let plugins = tempdir().unwrap();
            let stranded = plugins.path().join("stranded");
            std::fs::create_dir(&stranded).unwrap();
            for (file, bytes) in files {
                std::fs::write(stranded.join(file), bytes).unwrap();
            }
            let stale = plugins.path().join(".stranded.installing-4242");
            std::fs::create_dir(&stale).unwrap();
            std::fs::write(stale.join("manifest.toml"), manifest).unwrap();
            // Staging owned by other names: another package, and one whose
            // name merely extends this one.
            let unrelated = plugins.path().join(".other.installing-4242");
            let extended = plugins
                .path()
                .join(".stranded.installing-x.installing-4242");
            std::fs::create_dir(&unrelated).unwrap();
            std::fs::create_dir(&extended).unwrap();
            // A staging-shaped symlink is never followed, nor removed.
            #[cfg(unix)]
            let (linked_staging, link_target) = {
                let target = tempdir().unwrap();
                std::fs::write(target.path().join("keep.txt"), "kept").unwrap();
                let link = plugins.path().join(".stranded.installing-4243");
                std::os::unix::fs::symlink(target.path(), &link).unwrap();
                (link, target)
            };

            let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
            assert!(host.get_plugin("stranded").is_none(), "{shape}: discovered");
            let before = package_bytes(&stranded);

            let err = host
                .install(source.path().to_str().unwrap())
                .expect_err("the stranded directory must block the install");
            assert!(
                matches!(err, PluginError::UnadmittedPackage { ref name, .. } if name == "stranded"),
                "{shape}: {err}"
            );
            assert_eq!(package_bytes(&stranded), before, "{shape}: install wrote");
            assert!(stale.is_dir(), "{shape}: install swept staging");

            host.remove("stranded")
                .unwrap_or_else(|err| panic!("{shape}: remove must recover it: {err}"));
            assert!(!stranded.exists(), "{shape}: the directory survived");
            assert!(
                stale.exists(),
                "{shape}: ambiguous legacy staging must be retained"
            );
            assert!(
                unrelated.is_dir() && extended.is_dir(),
                "{shape}: over-swept"
            );
            #[cfg(unix)]
            {
                assert!(
                    std::fs::symlink_metadata(&linked_staging)
                        .unwrap()
                        .file_type()
                        .is_symlink(),
                    "{shape}: a staging-shaped symlink was removed"
                );
                assert!(link_target.path().join("keep.txt").is_file());
            }

            assert_eq!(
                host.install(source.path().to_str().unwrap()).unwrap(),
                "stranded",
                "{shape}: the retry must install"
            );
            assert_eq!(package_bytes(&stranded), package_bytes(source.path()));
        }
    }

    /// Recovery deletes only what an install could have left. A broken
    /// manifest next to anything else, a plugin's source checkout whose
    /// component is not built, a stray file beside the component, a directory
    /// where the component goes, or skill drafts next to a manifest without the
    /// skill capability is kept, every byte of it.
    #[test]
    fn remove_keeps_a_directory_holding_files_an_install_never_writes() {
        let unbuilt = "name = \"devtool\"\nversion = \"0.1.0\"\nwasm_path = \"target/plugin.wasm\"\ncapabilities = [\"tool\"]\n";
        let mismatched = format!("{unbuilt}wasm_sha256 = \"{}\"\n", "0".repeat(64));
        for (label, files) in [
            (
                "broken manifest and a note",
                vec![
                    ("manifest.toml", "name =".to_string()),
                    ("notes.txt", "keep".to_string()),
                ],
            ),
            (
                "source checkout, component not built",
                vec![
                    ("manifest.toml", unbuilt.to_string()),
                    ("Cargo.toml", "[package]".to_string()),
                    ("src/lib.rs", "fn main() {}".to_string()),
                    (".git/HEAD", "ref: refs/heads/main".to_string()),
                ],
            ),
            (
                "a stray file beside the component",
                vec![
                    ("manifest.toml", mismatched.clone()),
                    ("target/plugin.wasm", "\0asm".to_string()),
                    ("target/README", "build output".to_string()),
                ],
            ),
            (
                "a directory where the component goes",
                vec![
                    ("manifest.toml", unbuilt.to_string()),
                    ("target/plugin.wasm/inner", "not a component".to_string()),
                ],
            ),
            (
                "skill drafts next to a tool manifest",
                vec![
                    ("manifest.toml", unbuilt.to_string()),
                    ("skills/setup/SKILL.md", "draft".to_string()),
                ],
            ),
        ] {
            let plugins = tempdir().unwrap();
            let dir = plugins.path().join("devtool");
            for (path, contents) in &files {
                let path = dir.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, contents).unwrap();
            }
            let before = nested_package_bytes(&dir);
            let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
            let err = host.remove("devtool").expect_err(label);
            // Next to a manifest that does not parse, nothing at all can be an
            // interrupted install's, and the reason says so.
            let expected = if files[0].1 == "name =" {
                "its manifest.toml does not parse, so nothing beside it can be an interrupted install's, yet it holds '"
            } else {
                "it holds files an install never writes, such as '"
            };
            assert!(
                matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.starts_with(expected)),
                "{label}: {err}"
            );
            assert_eq!(nested_package_bytes(&dir), before, "{label}");
        }
    }

    /// What an install writes is still recoverable when the component sits
    /// below a directory: the manifest, the component's folder and file, and,
    /// for a skill plugin, `skills/`.
    #[test]
    fn remove_recovers_a_broken_package_whose_component_is_nested() {
        let plugins = tempdir().unwrap();
        let dir = plugins.path().join("nested");
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("skills/alpha")).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            format!(
                "name = \"nested\"\nversion = \"0.1.0\"\nwasm_path = \"target/plugin.wasm\"\ncapabilities = [\"tool\", \"skill\"]\nwasm_sha256 = \"{}\"\n",
                "0".repeat(64)
            ),
        )
        .unwrap();
        std::fs::write(dir.join("target/plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(dir.join("skills/alpha/notes.md"), b"partial").unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        host.remove("nested").unwrap();
        assert!(!dir.exists());
    }

    /// A remove that cannot take the identity of the package it claimed puts
    /// the package back and refuses, naming why, rather than failing with the
    /// package hidden in its claim.
    #[test]
    fn remove_puts_back_a_package_whose_claimed_identity_cannot_be_taken() {
        let plugins = tempdir().unwrap();
        let dir = plugins.path().join("broken");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            format!(
                "name = \"broken\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\nwasm_sha256 = \"{}\"\n",
                "0".repeat(64)
            ),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"\0asm").unwrap();
        let before = package_bytes(&dir);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        identity_fault::arm();
        let error = host.remove("broken").unwrap_err();
        assert!(
            matches!(&error, PluginError::UnadmittedPackage { reason, .. } if reason.contains("injected identity fault")),
            "{error}"
        );
        assert_eq!(package_bytes(&dir), before);
        assert_eq!(
            dir_entries(plugins.path()),
            [".zeroclaw-package-lock-v1", "broken"]
        );

        host.remove("broken").unwrap();
        assert!(!dir.exists());
    }

    /// A bare manifest file name is a package in the current directory: its
    /// parent is the empty path, which no directory handle opens.
    #[test]
    fn a_bare_manifest_file_name_names_the_current_directory() {
        assert_eq!(
            manifest_source_dir(Path::new("manifest.toml")).unwrap(),
            PathBuf::from(".")
        );
        assert_eq!(
            manifest_source_dir(Path::new("pkg/manifest.toml")).unwrap(),
            PathBuf::from("pkg")
        );
        let current = manifest_source_dir(Path::new("manifest.toml")).unwrap();
        assert!(Dir::open_ambient_dir(&current, cap_std::ambient_authority()).is_ok());
    }

    /// Every installer writes a package under its manifest's name, so a
    /// broken package whose manifest names another one was not left by an
    /// install of this name, such as a copy kept under another name. It is
    /// kept, every byte of it.
    #[test]
    fn remove_keeps_a_package_whose_manifest_names_another_package() {
        let plugins = tempdir().unwrap();
        let dir = plugins.path().join("demo-copy");
        std::fs::create_dir(&dir).unwrap();
        write_tool_source(&dir, "demo", b"\0asm partial");
        std::fs::remove_file(dir.join("plugin.wasm")).unwrap();
        let before = nested_package_bytes(&dir);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let err = host
            .remove("demo-copy")
            .expect_err("another package's manifest");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason == "its manifest names 'demo', and an install writes each package under its own name"),
            "{err}"
        );
        assert_eq!(nested_package_bytes(&dir), before);
    }

    /// No install writes a symlink: neither as the component, nor inside a
    /// skill bundle, whose copy takes only directories and regular files. A
    /// broken package holding one is kept, and the link's target is never
    /// touched.
    #[cfg(unix)]
    #[test]
    fn remove_keeps_a_package_holding_a_symlink_an_install_never_writes() {
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("build.wasm"), b"\0asm outside").unwrap();
        for (label, link, expected) in [
            ("a linked component", "plugin.wasm", "plugin.wasm"),
            (
                "a link in the skill bundle",
                "skills/alpha/link.md",
                "skills/alpha/link.md",
            ),
        ] {
            let plugins = tempdir().unwrap();
            let dir = plugins.path().join("linked");
            std::fs::create_dir_all(dir.join("skills/alpha")).unwrap();
            std::fs::write(
                dir.join("manifest.toml"),
                "name = \"linked\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\", \"skill\"]\n",
            )
            .unwrap();
            std::fs::write(dir.join("skills/alpha/SKILL.md"), b"# Alpha").unwrap();
            std::os::unix::fs::symlink(outside.path().join("build.wasm"), dir.join(link)).unwrap();
            let before = nested_package_bytes(&dir);
            let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
            let err = host.remove("linked").expect_err(label);
            assert!(
                matches!(&err, PluginError::UnadmittedPackage { reason, .. } if *reason == format!("it holds files an install never writes, such as '{expected}'")),
                "{label}: {err}"
            );
            assert_eq!(nested_package_bytes(&dir), before, "{label}");
            assert!(
                std::fs::symlink_metadata(dir.join(link))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        assert_eq!(
            std::fs::read(outside.path().join("build.wasm")).unwrap(),
            b"\0asm outside"
        );
    }

    /// A manifest-less directory is deleted only when it is empty. Every
    /// installer wrote `manifest.toml` right after creating the directory, so
    /// one that holds anything, or whose `manifest.toml` is not a file, was
    /// never an install: it is refused with its reason and nothing is swept.
    #[test]
    fn remove_keeps_directories_no_install_left() {
        for (label, occupant) in [
            ("files but no manifest", "notes"),
            ("a manifest.toml that is a directory", "odd"),
        ] {
            let plugins = tempdir().unwrap();
            let dir = plugins.path().join(occupant);
            if occupant == "notes" {
                std::fs::create_dir_all(dir.join("drafts")).unwrap();
                std::fs::write(dir.join("todo.txt"), "keep me").unwrap();
                std::fs::write(dir.join("drafts").join("a.md"), "draft").unwrap();
            } else {
                std::fs::create_dir_all(dir.join("manifest.toml")).unwrap();
            }
            let staging = plugins.path().join(format!(".{occupant}.installing-4242"));
            std::fs::create_dir(&staging).unwrap();
            let before = dir_entries(&dir);

            let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
            let err = host
                .remove(occupant)
                .expect_err("a directory no install left must be kept");
            let PluginError::UnadmittedPackage { name, reason } = &err else {
                panic!("{label}: {err}");
            };
            assert_eq!(name, occupant);
            let expected = if occupant == "notes" {
                "it holds files but no manifest.toml, so no interrupted install left it"
            } else {
                "its manifest.toml is not a regular file"
            };
            assert_eq!(reason, expected, "{label}");
            assert_eq!(dir_entries(&dir), before, "{label}: contents changed");
            assert!(staging.is_dir(), "{label}: staging was swept");
        }
    }

    /// A directory recovery cannot inspect or list is refused before anything
    /// is swept. At 0o600 its manifest cannot be looked up, which is not the
    /// same as missing. At 0o300 the manifest reads and admission rejects the
    /// package, but the directory cannot be listed, so a delete would fail
    /// after the sweep.
    #[cfg(unix)]
    #[test]
    fn remove_refuses_a_directory_it_cannot_inspect_and_sweeps_nothing() {
        use std::os::unix::fs::PermissionsExt;

        for (mode, expected) in [
            (0o600, "its manifest.toml cannot be inspected ("),
            (0o300, "it cannot be listed"),
        ] {
            let plugins = tempdir().unwrap();
            let locked = plugins.path().join("locked");
            std::fs::create_dir(&locked).unwrap();
            // No capabilities: a shape admission rejects as the package's own.
            std::fs::write(
                locked.join("manifest.toml"),
                "name = \"locked\"\nversion = \"0.1.0\"\ncapabilities = []\n",
            )
            .unwrap();
            let staging = plugins.path().join(".locked.installing-7");
            std::fs::create_dir(&staging).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(mode)).unwrap();
            let restore = || {
                std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            if std::fs::read_dir(&locked).is_ok()
                && std::fs::symlink_metadata(locked.join("manifest.toml")).is_ok()
            {
                // Running as root: permissions hide nothing.
                restore();
                return;
            }

            let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
            let err = host.remove("locked");
            restore();

            let Err(PluginError::UnadmittedPackage { reason, .. }) = &err else {
                panic!("{mode:o}: an uninspectable directory must be refused: {err:?}");
            };
            assert!(reason.starts_with(expected), "{mode:o}: {reason}");
            assert!(staging.is_dir(), "{mode:o}: staging was swept");
            assert_eq!(dir_entries(&locked), ["manifest.toml"], "{mode:o}");
        }
    }

    fn nested_package_bytes(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut result = Vec::new();
        for name in dir_entries(dir) {
            let path = dir.join(&name);
            if path.is_dir() {
                for (child, bytes) in nested_package_bytes(&path) {
                    result.push((Path::new(&name).join(child), bytes));
                }
            } else {
                result.push((PathBuf::from(name), std::fs::read(path).unwrap()));
            }
        }
        result
    }

    #[cfg(unix)]
    #[test]
    fn remove_refuses_inaccessible_nested_skill_and_preserves_all_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let plugins = tempdir().unwrap();
        // Discover before the package appears so remove takes the unloaded path.
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        write_skill_bundle_plugin(plugins.path(), "locked-skill", &["nested"]);
        let package = plugins.path().join("locked-skill");
        std::fs::write(package.join("sentinel"), b"readable sibling must survive").unwrap();
        let before = nested_package_bytes(&package);
        let stage = host
            .root()
            .unwrap()
            .transaction("locked-skill", "installing")
            .unwrap();
        stage.dir.create_dir(recovery::PACKAGE).unwrap();
        stage
            .dir
            .write(
                Path::new(recovery::PACKAGE).join("sentinel"),
                b"protocol-stage bytes must survive",
            )
            .unwrap();
        let protocol_stage = plugins.path().join(&stage.entry).join(recovery::PACKAGE);
        drop(stage); // A real abandoned generation would be eligible after recovery.
        let staging = plugins.path().join(".locked-skill.installing-7");
        let unrelated = plugins.path().join(".other.installing-7");
        for stage in [&staging, &unrelated] {
            std::fs::create_dir(stage).unwrap();
            std::fs::write(stage.join("sentinel"), b"stage bytes must survive").unwrap();
        }
        let nested = package.join("skills/nested");
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o600)).unwrap();
        let probe = std::fs::symlink_metadata(nested.join("SKILL.md"));
        if probe.is_ok() {
            // Privileged identities may bypass DAC. Do not count that as proof
            // that recovery refused a real permission error.
            std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("permission control inapplicable: identity can inspect mode-0600 child");
            return;
        }
        let source_result = host.admit_source(package.to_str().unwrap());
        let remove_result = host.remove("locked-skill");
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            probe.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(
            matches!(source_result, Err(PluginError::Io(ref error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
        );
        assert!(
            matches!(remove_result, Err(PluginError::UnadmittedPackage { .. })),
            "{remove_result:?}"
        );
        assert_eq!(nested_package_bytes(&package), before);
        assert_eq!(
            std::fs::read(protocol_stage.join("sentinel")).unwrap(),
            b"protocol-stage bytes must survive"
        );
        for stage in [&staging, &unrelated] {
            assert_eq!(
                std::fs::read(stage.join("sentinel")).unwrap(),
                b"stage bytes must survive"
            );
        }
    }

    #[test]
    fn remove_recovers_missing_skill_md_but_refuses_valid_skill_bundle() {
        for missing in [false, true] {
            let plugins = tempdir().unwrap();
            let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
            write_skill_bundle_plugin(plugins.path(), "bundle", &["nested"]);
            let package = plugins.path().join("bundle");
            if missing {
                std::fs::remove_file(package.join("skills/nested/SKILL.md")).unwrap();
            }
            let before = nested_package_bytes(&package);
            let result = host.remove("bundle");
            if missing {
                result.expect("missing skill file is structural recovery evidence");
                assert!(!package.exists());
            } else {
                assert!(matches!(result, Err(PluginError::UnadmittedPackage { .. })));
                assert_eq!(nested_package_bytes(&package), before);
            }
        }
    }

    /// A file at a package name is not a package directory. Install refuses
    /// to overwrite it, and `remove` says why it will not delete it instead of
    /// answering "not found".
    #[test]
    fn a_file_at_the_package_name_is_refused_by_install_and_remove() {
        let plugins = tempdir().unwrap();
        let file = plugins.path().join("filey");
        std::fs::write(&file, "not a package").unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        let source = tempdir().unwrap();
        write_tool_source(source.path(), "filey", b"\0asm");
        let err = host
            .install(source.path().to_str().unwrap())
            .expect_err("install must not overwrite a file");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("not a directory")),
            "{err}"
        );

        let err = host
            .remove("filey")
            .expect_err("remove must not delete a file");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("not a directory")),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not a package");
    }

    /// A package the host did not load is kept when admission accepts it or
    /// rejects it for its signature, and the refusal carries admission's own
    /// verdict: one a strict signature policy rejects, and one that appeared
    /// after the host looked.
    #[test]
    fn remove_refuses_a_valid_package_it_did_not_load() {
        let dir = tempdir().unwrap();
        write_unsigned_tool_plugin(dir.path(), "unsigned-tool");
        let package = dir.path().join("unsigned-tool");
        let before = package_bytes(&package);
        let mut strict = PluginHost::from_plugins_dir_with_security(
            dir.path(),
            SignatureMode::Strict,
            Vec::new(),
        )
        .unwrap();
        assert!(strict.get_plugin("unsigned-tool").is_none());

        let err = strict
            .remove("unsigned-tool")
            .expect_err("a package failing only trust policy is not incomplete");
        assert!(
            matches!(
                &err,
                PluginError::UnadmittedPackage { name, reason }
                    if name == "unsigned-tool" && reason.contains("unsigned")
            ),
            "{err}"
        );
        assert_eq!(package_bytes(&package), before);

        let later = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(later.path()).unwrap();
        write_unsigned_tool_plugin(later.path(), "late-tool");
        let err = host
            .remove("late-tool")
            .expect_err("a package that admits cleanly is not incomplete");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason == "admission accepts this package"),
            "{err}"
        );
        assert!(later.path().join("late-tool/plugin.wasm").is_file());
    }

    /// Discovery loads a package under the name its manifest declares, from
    /// whatever directory holds it. Removing the directory's own name must not
    /// delete a loaded package's files, even once its manifest no longer
    /// parses, which admission alone would count as the directory's own
    /// defect.
    #[test]
    fn remove_keeps_a_directory_that_holds_a_loaded_package() {
        let dir = tempdir().unwrap();
        let misnamed = dir.path().join("misnamed");
        std::fs::create_dir(&misnamed).unwrap();
        write_tool_source(&misnamed, "declared", b"\0asm");
        let mut host = PluginHost::from_plugins_dir(dir.path()).unwrap();
        assert!(host.get_plugin("declared").is_some());

        let err = host
            .remove("misnamed")
            .expect_err("a loaded package's directory is kept");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("'declared'")),
            "{err}"
        );

        std::fs::write(misnamed.join("manifest.toml"), "name = ").unwrap();
        let err = host
            .remove("misnamed")
            .expect_err("a loaded package's directory is kept whatever its manifest reads");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("'declared'")),
            "{err}"
        );
        assert!(misnamed.join("plugin.wasm").is_file());
        assert!(host.get_plugin("declared").is_some());

        // A copy that appeared later elsewhere and names the loaded package is
        // kept too: admission answers `AlreadyLoaded` before it reads the
        // missing component, and that verdict is not the copy's own defect.
        let copy = dir.path().join("copy");
        std::fs::create_dir(&copy).unwrap();
        std::fs::write(
            copy.join("manifest.toml"),
            "name = \"declared\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        let err = host
            .remove("copy")
            .expect_err("a copy naming a loaded package is kept");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("already loaded")),
            "{err}"
        );
        assert!(copy.join("manifest.toml").is_file());
    }

    /// The loaded-directory guard compares file identity, not spelling. On a
    /// case-insensitive file system a lowercase slug reaches a directory
    /// spelled in another case, and that is still the loaded package's
    /// directory, even after its manifest stops parsing.
    #[cfg(unix)]
    #[test]
    fn remove_keeps_a_loaded_package_directory_reached_through_another_case() {
        let dir = tempdir().unwrap();
        let cased = dir.path().join("Casey");
        std::fs::create_dir(&cased).unwrap();
        write_tool_source(&cased, "casedecl", b"\0asm");
        if !dir.path().join("casey").is_dir() {
            // A case-sensitive file system: `casey` names nothing, so there
            // is no second spelling to guard against.
            return;
        }
        let mut host = PluginHost::from_plugins_dir(dir.path()).unwrap();
        assert!(host.get_plugin("casedecl").is_some());

        std::fs::write(cased.join("manifest.toml"), "name = ").unwrap();
        let err = host
            .remove("casey")
            .expect_err("a loaded package's directory is kept under any spelling");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("'casedecl' is loaded from it")),
            "{err}"
        );
        assert!(cased.join("plugin.wasm").is_file());
    }

    /// Recovery never follows a symlinked package root, even to a directory
    /// it would recover if it were real, and sweeps nothing for that name.
    #[cfg(unix)]
    #[test]
    fn remove_refuses_a_symlinked_package_dir() {
        use std::os::unix::fs::symlink;

        let plugins = tempdir().unwrap();
        let external = tempdir().unwrap();
        std::fs::write(
            external.path().join("manifest.toml"),
            "name = \"linked\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        let link = plugins.path().join("linked");
        symlink(external.path(), &link).unwrap();
        let staging = plugins.path().join(".linked.installing-4242");
        std::fs::create_dir(&staging).unwrap();

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let err = host
            .remove("linked")
            .expect_err("a symlinked package root must be refused");
        assert!(
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("symlink")),
            "{err}"
        );
        assert!(external.path().join("manifest.toml").is_file());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(staging.is_dir());
    }

    /// Only a package slug reaches the file system. Each target is an empty
    /// directory, which recovery deletes, so the name check is what keeps it.
    /// The plugins directory and its parent (`""`, `.`, `..`) are not empty
    /// either, which would refuse them a second time.
    #[test]
    fn remove_rejects_a_non_slug_name() {
        let root = tempdir().unwrap();
        let plugins = root.path().join("plugins");
        let mut host = PluginHost::from_plugins_dir(&plugins).unwrap();
        let targets = [
            root.path().join("outside"),
            plugins.join(".hidden.installing-4242"),
            plugins.join("Upper"),
            plugins.join("nested").join("dir"),
        ];
        for target in &targets {
            std::fs::create_dir_all(target).unwrap();
        }

        for name in [
            "",
            ".",
            "..",
            "../outside",
            ".hidden.installing-4242",
            "Upper",
            "nested/dir",
        ] {
            let err = host
                .remove(name)
                .expect_err("a non-slug name must be refused");
            assert!(matches!(err, PluginError::NotFound(_)), "{name:?}: {err}");
        }
        assert!(targets.iter().all(|target| target.is_dir()));
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
            Err(PluginError::NamespaceChanged(_))
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
        assert!(matches!(read, Err(PluginError::NamespaceChanged(_))));
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
        assert!(matches!(read, Err(PluginError::NamespaceChanged(_))));
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

    /// Run `discover` with `hook` called at every discovery step reached on
    /// this thread.
    #[cfg(unix)]
    fn with_discovery_steps<T>(
        hook: impl FnMut(DiscoveryStep) + 'static,
        discover: impl FnOnce() -> T,
    ) -> T {
        DISCOVERY_STEP_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        let discovered = discover();
        DISCOVERY_STEP_HOOK.with(|slot| *slot.borrow_mut() = None);
        discovered
    }

    /// Write a tool package at `dir`: `version` and the component bytes tell
    /// two generations of the same package apart.
    fn write_tool_package(dir: &Path, version: &str, wasm: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            format!(
                "name = \"pkg\"\nversion = \"{version}\"\ncapabilities = [\"tool\"]\nwasm_path = \"plugin.wasm\"\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.wasm"), wasm).unwrap();
    }

    /// Admission proves the manifest and the component it admits are one
    /// generation. Here the package is republished between the manifest read
    /// and the admission, so the old policy would describe the new bytes; the
    /// read must fail closed instead.
    #[test]
    fn admit_component_refuses_a_package_republished_since_its_manifest_was_read() {
        let root = tempdir().unwrap();
        let package = root.path().join("pkg");
        write_tool_package(&package, "0.1.0", b"\0asm\x01");
        let manifest_toml = std::fs::read_to_string(package.join("manifest.toml")).unwrap();
        let manifest: PluginManifest = toml::from_str(&manifest_toml).unwrap();

        std::fs::remove_dir_all(&package).unwrap();
        write_tool_package(&package, "0.2.0", b"\0asm\x02");
        assert_ne!(
            std::fs::read_to_string(package.join("manifest.toml")).unwrap(),
            manifest_toml,
            "the replacement must publish another manifest"
        );

        let admitted = admit_component(&package, &manifest, &manifest_toml);

        assert!(
            matches!(admitted, Err(PluginError::NamespaceChanged(_))),
            "the republished package must be refused, got {:?}",
            admitted.map(|admitted| admitted.map(|admitted| admitted.bytes().len()))
        );
    }

    /// The same package, unchanged, is admitted: the generation check refuses
    /// republication, not admission itself.
    #[test]
    fn admit_component_admits_an_unchanged_package() {
        let root = tempdir().unwrap();
        let package = root.path().join("pkg");
        write_tool_package(&package, "0.1.0", b"\0asm\x01");
        let manifest_toml = std::fs::read_to_string(package.join("manifest.toml")).unwrap();
        let manifest: PluginManifest = toml::from_str(&manifest_toml).unwrap();

        let admitted = admit_component(&package, &manifest, &manifest_toml)
            .unwrap()
            .expect("the package names a component");

        assert_eq!(admitted.bytes(), b"\0asm\x01");
    }

    /// Discovery reads a package's manifest by pathname, then admits its
    /// component through a confined handle. A republication in between must not
    /// let the replacement's bytes run under the manifest that was read, so
    /// discovery skips the package.
    #[cfg(unix)]
    #[test]
    fn discovery_skips_a_package_republished_between_its_manifest_and_admission() {
        let root = tempdir().unwrap();
        let plugins_dir = root.path().join("plugins");
        let package = plugins_dir.join("pkg");
        write_tool_package(&package, "0.1.0", b"\0asm\x01");
        // Discovery ignores hidden entries, so the replacement can wait
        // beside the package until the step under test.
        let next = plugins_dir.join(".next").join("pkg");
        write_tool_package(&next, "0.2.0", b"\0asm\x02");

        let replaced = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let swapped = std::sync::Arc::clone(&replaced);
        let host = with_discovery_steps(
            move |step| {
                assert_eq!(step, DiscoveryStep::BeforeComponentAdmission);
                std::fs::remove_dir_all(&package).unwrap();
                std::fs::rename(&next, &package).unwrap();
                swapped.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            || {
                PluginHost::from_plugins_dir_with_security(
                    &plugins_dir,
                    SignatureMode::Disabled,
                    Vec::new(),
                )
            },
        )
        .unwrap();

        assert!(
            replaced.load(std::sync::atomic::Ordering::SeqCst),
            "the swap must run during admission"
        );
        assert!(
            host.list_plugins().is_empty(),
            "a republished package must not be admitted: {:?}",
            host.list_plugins()
        );
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
                matches!(read, Err(PluginError::NamespaceChanged(_))),
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
            matches!(read, Err(PluginError::NamespaceChanged(_))),
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

    /// An unavailable no-replace primitive refuses publication and recovery.
    /// Install then clears the copy it staged, so a retry, which fails the same
    /// way, leaves nothing behind either, and every final package keeps its
    /// bytes.
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn install_and_recovery_refuse_without_a_no_replace_rename() {
        struct Reset(Option<rustix::io::Errno>);
        impl Drop for Reset {
            fn drop(&mut self) {
                recovery::FORCE_RENAME_ERROR.with(|error| error.set(self.0));
            }
        }
        let _reset = Reset(
            recovery::FORCE_RENAME_ERROR
                .with(|error| error.replace(Some(rustix::io::Errno::NOSYS))),
        );
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let source = tempdir().unwrap();
        write_tool_source(source.path(), "plain", b"\0asm plain");
        let expected = package_bytes(source.path());
        for _ in 0..2 {
            let result = host.install(source.path().to_str().unwrap());
            assert!(
                matches!(&result, Err(PluginError::MoveUnsupported(reason)) if reason.contains("no-replace")),
                "an unsupported publication must clear its stage: {result:?}"
            );
            assert_eq!(dir_entries(plugins.path()), [".zeroclaw-package-lock-v1"]);
        }

        let broken = plugins.path().join("broken");
        std::fs::create_dir(&broken).unwrap();
        std::fs::write(broken.join("manifest.toml"), "name =").unwrap();
        let broken_before = package_bytes(&broken);
        let broken_id = same_file::Handle::from_path(&broken).unwrap();
        assert!(matches!(
            host.remove("broken"),
            Err(PluginError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert_eq!(package_bytes(&broken), broken_before);
        assert_eq!(same_file::Handle::from_path(&broken).unwrap(), broken_id);

        let late = plugins.path().join("late");
        std::fs::create_dir(&late).unwrap();
        write_tool_source(&late, "late", b"\0asm late");
        let before = package_bytes(&late);
        assert!(matches!(
            host.remove("late"),
            Err(PluginError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert_eq!(package_bytes(&late), before);
        assert_eq!(package_bytes(source.path()), expected);
    }

    /// A lock file this user cannot write, such as one another user created in
    /// a shared plugins directory, still coordinates install and remove.
    #[cfg(unix)]
    #[test]
    fn a_lock_file_this_user_cannot_write_still_coordinates() {
        use std::os::unix::fs::PermissionsExt;

        let plugins = tempdir().unwrap();
        let lock = plugins.path().join(".zeroclaw-package-lock-v1");
        std::fs::write(&lock, b"").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let source = tempdir().unwrap();
        write_tool_source(source.path(), "shared", b"\0asm shared");
        assert_eq!(
            host.install(source.path().to_str().unwrap()).unwrap(),
            "shared"
        );
        host.remove("shared").unwrap();
        assert_eq!(dir_entries(plugins.path()), [".zeroclaw-package-lock-v1"]);
    }

    /// Only package operations open the plugins root. A host that only
    /// discovers holds none of it, so on Windows a daemon's hosts do not pin
    /// the plugins directory or its ancestors.
    #[test]
    fn only_a_package_operation_opens_the_plugins_root() {
        let plugins = tempdir().unwrap();
        write_unsigned_tool_plugin(plugins.path(), "listed");
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.get_plugin("listed").is_some());
        assert!(host.recovery_root.get().is_none());
        assert!(!plugins.path().join(".zeroclaw-package-lock-v1").exists());
        assert!(matches!(
            host.remove("absent"),
            Err(PluginError::NotFound(_))
        ));
        assert!(host.recovery_root.get().is_some());
    }

    /// Run `admit` on its own thread, and fail rather than hang if it blocks.
    #[cfg(unix)]
    fn without_blocking<T: Send + 'static>(admit: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || sender.send(admit()).unwrap());
        match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("a FIFO in place of the payload blocked admission")
            }
            // The worker panicked before sending: report that failure rather
            // than a blocked admission.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::panic::resume_unwind(
                worker
                    .join()
                    .expect_err("the worker sends before it returns"),
            ),
        }
    }

    #[cfg(unix)]
    fn make_fifo(path: &Path) {
        assert!(
            std::process::Command::new("mkfifo")
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }

    /// Install reads the component through its handle on the source
    /// directory, as a confined read does, so a FIFO in its place fails at once
    /// instead of blocking the open.
    #[cfg(unix)]
    #[test]
    fn admit_source_refuses_a_fifo_payload_without_blocking() {
        let source = tempdir().unwrap();
        write_tool_source(source.path(), "piped", b"unused");
        std::fs::remove_file(source.path().join("plugin.wasm")).unwrap();
        make_fifo(&source.path().join("plugin.wasm"));
        let plugins = tempdir().unwrap();
        let plugins_dir = plugins.path().to_path_buf();
        let source_dir = source.path().to_str().unwrap().to_owned();

        let admitted = without_blocking(move || {
            PluginHost::from_plugins_dir(&plugins_dir)
                .unwrap()
                .admit_source(&source_dir)
                .map(|_| ())
        });
        assert!(
            matches!(&admitted, Err(PluginError::InvalidManifest(message)) if message.contains("not a regular file")),
            "{admitted:?}"
        );
    }

    /// Recovery judges a claimed package through the same walk. A FIFO payload
    /// fails admission at once instead of blocking `remove` while it holds the
    /// package lock. No install writes a FIFO, so the package is kept, and the
    /// next install takes the lock at once.
    #[cfg(unix)]
    #[test]
    fn remove_keeps_a_package_whose_payload_is_a_fifo_without_blocking() {
        use std::os::unix::fs::FileTypeExt;

        let plugins = tempdir().unwrap();
        let package = plugins.path().join("piped");
        std::fs::create_dir(&package).unwrap();
        write_tool_source(&package, "piped", b"unused");
        std::fs::remove_file(package.join("plugin.wasm")).unwrap();
        make_fifo(&package.join("plugin.wasm"));
        let plugins_dir = plugins.path().to_path_buf();

        let removed = without_blocking(move || {
            PluginHost::from_plugins_dir(&plugins_dir)
                .unwrap()
                .remove("piped")
        });
        assert!(
            matches!(&removed, Err(PluginError::UnadmittedPackage { reason, .. }) if reason == "it holds files an install never writes, such as 'plugin.wasm'"),
            "{removed:?}"
        );
        assert!(package.join("manifest.toml").is_file());
        assert!(
            std::fs::symlink_metadata(package.join("plugin.wasm"))
                .unwrap()
                .file_type()
                .is_fifo()
        );

        let source = tempdir().unwrap();
        write_tool_source(source.path(), "other", b"\0asm other");
        let plugins_dir = plugins.path().to_path_buf();
        let source_dir = source.path().to_str().unwrap().to_owned();
        let installed = without_blocking(move || {
            PluginHost::from_plugins_dir(&plugins_dir)
                .unwrap()
                .install(&source_dir)
        });
        assert_eq!(installed.unwrap(), "other");
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

    fn write_tool_version(dir: &Path, name: &str, version: &str, wasm: &[u8]) {
        std::fs::write(
            dir.join("manifest.toml"),
            format!(
                "name = \"{name}\"\nversion = \"{version}\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.wasm"), wasm).unwrap();
    }

    fn install_tool_version(host: &mut PluginHost, name: &str, version: &str, wasm: &[u8]) {
        let source = tempdir().unwrap();
        write_tool_version(source.path(), name, version, wasm);
        host.install(source.path().to_str().unwrap()).unwrap();
    }

    /// Admit a tool source for `name` at `version` and replace it.
    fn update_tool(
        host: &mut PluginHost,
        name: &str,
        version: &str,
        wasm: &[u8],
    ) -> Result<ReplacedPackage, PluginError> {
        let source = tempdir().unwrap();
        write_tool_version(source.path(), name, version, wasm);
        let admitted = host.admit_update(name, source.path().to_str().unwrap())?;
        host.update_admitted(admitted)
    }

    /// Write a tool source whose signed manifest binds the component digest:
    /// the shape strict signature policy admits.
    fn write_signed_tool_version(
        dir: &Path,
        name: &str,
        version: &str,
        wasm: &[u8],
        private_key: &[u8],
        publisher_key: &str,
    ) {
        let unsigned = format!(
            "name = \"{name}\"\nversion = \"{version}\"\nwasm_path = \"plugin.wasm\"\nwasm_sha256 = \"{}\"\ncapabilities = [\"tool\"]\n",
            signature::sha256_hex(wasm)
        );
        let signed_value = signature::sign_manifest(&unsigned, private_key).unwrap();
        let signed = unsigned.replacen(
            "wasm_path = \"plugin.wasm\"",
            &format!(
                "signature = \"{signed_value}\"\npublisher_key = \"{publisher_key}\"\nwasm_path = \"plugin.wasm\""
            ),
            1,
        );
        std::fs::write(dir.join("manifest.toml"), signed).unwrap();
        std::fs::write(dir.join("plugin.wasm"), wasm).unwrap();
    }

    /// Hidden entries of `dir` other than the package lock: stages and claims.
    fn hidden_entries(dir: &Path) -> Vec<String> {
        dir_entries(dir)
            .into_iter()
            .filter(|name| name.starts_with('.') && name != ".zeroclaw-package-lock-v1")
            .collect()
    }

    /// Leave `name` claimed by an update that stopped before publishing: its
    /// installed directory moved into a `replacing` transaction whose lease is
    /// released on return. With `marked`, the claim is already marked for
    /// deletion, as a finished update leaves one it could not delete.
    /// Returns the transaction's entry.
    fn abandon_claim(host: &PluginHost, name: &str, marked: bool) -> String {
        let root = host.root().unwrap();
        let tx = root.transaction(name, "replacing").unwrap();
        tx.claim(root, name).unwrap();
        if marked {
            tx.mark_deleting().unwrap();
        }
        tx.entry.clone()
    }

    /// A host that already cached the old generation must refresh even when
    /// another host completed its update and released every claim.
    #[test]
    fn recovery_refreshes_preloaded_generation_after_completed_update_without_claims() {
        let plugins = tempdir().unwrap();
        let mut stale = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut stale, "weather", "1.0.0", b"\0asm v1");
        let mut updater = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let replaced = update_tool(&mut updater, "weather", "2.0.0", b"\0asm v2").unwrap();
        assert_eq!(replaced.previous_version, "1.0.0");
        assert!(replaced.leftover.is_none());
        assert!(hidden_entries(plugins.path()).is_empty());
        assert_eq!(stale.manifest("weather").unwrap().version, "1.0.0");
        assert_eq!(
            stale.admitted_component("weather").unwrap().bytes(),
            b"\0asm v1"
        );

        assert_eq!(
            stale.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Nothing
        );
        assert_eq!(stale.manifest("weather").unwrap().version, "2.0.0");
        assert_eq!(
            stale.admitted_component("weather").unwrap().bytes(),
            b"\0asm v2"
        );
        assert_eq!(
            stale.loaded.get("weather").unwrap().manifest_toml,
            std::fs::read_to_string(plugins.path().join("weather/manifest.toml")).unwrap()
        );
        // The refreshed generation must also pass the next replacement's
        // identity comparison, instead of reporting NamespaceChanged.
        let replaced = update_tool(&mut stale, "weather", "3.0.0", b"\0asm v3").unwrap();
        assert_eq!(replaced.previous_version, "2.0.0");
        assert!(replaced.leftover.is_none());
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    #[test]
    fn recovery_forgets_preloaded_removed_package_without_claims() {
        let plugins = tempdir().unwrap();
        let mut stale = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut stale, "weather", "1.0.0", b"\0asm v1");
        install_tool_version(&mut stale, "kept", "1.0.0", b"\0asm kept");
        let mut remover = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        remover.remove("weather").unwrap();
        assert!(!plugins.path().join("weather").exists());
        assert!(hidden_entries(plugins.path()).is_empty());
        assert!(stale.manifest("weather").is_some());

        assert_eq!(
            stale.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Nothing
        );
        assert!(stale.manifest("weather").is_none());
        assert!(stale.get_plugin("weather").is_none());
        assert!(stale.admitted_component("weather").is_none());
        assert_eq!(stale.manifest("kept").unwrap().version, "1.0.0");
        assert_eq!(
            stale.admitted_component("kept").unwrap().bytes(),
            b"\0asm kept"
        );
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    #[test]
    fn recovery_without_claims_keeps_current_generation_and_missing_name_noop() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let before = package_bytes(&plugins.path().join("weather"));
        for name in ["weather", "missing"] {
            assert_eq!(
                host.recover_interrupted_update(name).unwrap(),
                UpdateRecovery::Nothing
            );
            assert_eq!(host.manifest("weather").unwrap().version, "1.0.0");
            assert_eq!(
                host.admitted_component("weather").unwrap().bytes(),
                b"\0asm v1"
            );
            assert!(host.manifest("missing").is_none());
            assert_eq!(host.list_plugins().len(), 1);
            assert_eq!(package_bytes(&plugins.path().join("weather")), before);
            assert!(hidden_entries(plugins.path()).is_empty());
        }
    }

    /// Recovery judges again only the package it was asked about: a batch
    /// refreshes each package when it reaches it, instead of admitting every
    /// installed package once per name.
    #[test]
    fn recovery_without_claims_refreshes_only_the_named_package() {
        let plugins = tempdir().unwrap();
        let mut stale = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut stale, "weather", "1.0.0", b"\0asm weather v1");
        install_tool_version(&mut stale, "tides", "1.0.0", b"\0asm tides v1");
        let mut updater = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        update_tool(&mut updater, "weather", "2.0.0", b"\0asm weather v2").unwrap();
        update_tool(&mut updater, "tides", "2.0.0", b"\0asm tides v2").unwrap();

        assert_eq!(
            stale.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Nothing
        );
        assert_eq!(stale.manifest("weather").unwrap().version, "2.0.0");
        assert_eq!(stale.manifest("tides").unwrap().version, "1.0.0");
        assert_eq!(
            stale.admitted_component("tides").unwrap().bytes(),
            b"\0asm tides v1"
        );

        assert_eq!(
            stale.recover_interrupted_update("tides").unwrap(),
            UpdateRecovery::Nothing
        );
        assert_eq!(stale.manifest("tides").unwrap().version, "2.0.0");
        assert_eq!(
            stale.admitted_component("tides").unwrap().bytes(),
            b"\0asm tides v2"
        );
        assert_eq!(stale.manifest("weather").unwrap().version, "2.0.0");
    }

    /// Judging one package again keeps discovery's rule for a name that two
    /// package directories claim: neither is admitted.
    #[test]
    fn recovery_refresh_admits_neither_package_of_a_name_two_directories_claim() {
        let plugins = tempdir().unwrap();
        let mut stale = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut stale, "weather", "1.0.0", b"\0asm v1");
        install_tool_version(&mut stale, "tides", "1.0.0", b"\0asm tides v1");
        let copy = plugins.path().join("weather-copy");
        std::fs::create_dir(&copy).unwrap();
        write_tool_version(&copy, "weather", "1.0.0", b"\0asm v1");
        let fresh = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(fresh.manifest("weather").is_none());

        assert_eq!(
            stale.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Nothing
        );
        assert!(stale.manifest("weather").is_none());
        assert!(stale.admitted_component("weather").is_none());
        assert_eq!(stale.manifest("tides").unwrap().version, "1.0.0");
    }

    /// An update replaces the package where it is installed: the new manifest
    /// and component are on disk, the host reports the new version, a fresh
    /// discovery admits it, and nothing but the package lock is left hidden.
    #[test]
    fn update_replaces_the_package_in_place_and_reports_the_previous_version() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        let replaced = update_tool(&mut host, "weather", "2.0.0", b"\0asm v2").unwrap();

        assert_eq!(replaced.previous_version, "1.0.0");
        assert!(replaced.leftover.is_none());
        assert_eq!(host.get_plugin("weather").unwrap().version, "2.0.0");
        assert_eq!(
            std::fs::read(plugins.path().join("weather/plugin.wasm")).unwrap(),
            b"\0asm v2"
        );
        assert!(hidden_entries(plugins.path()).is_empty());
        let rediscovered = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(rediscovered.get_plugin("weather").unwrap().version, "2.0.0");
        assert_eq!(
            rediscovered.admitted_component("weather").unwrap().bytes(),
            b"\0asm v2"
        );
    }

    /// Updating a package that is not installed is refused before the source
    /// is opened: the error names the plugin, not a missing manifest.
    #[test]
    fn admit_update_refuses_a_package_that_is_not_installed_before_opening_the_source() {
        let plugins = tempdir().unwrap();
        let host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        let err = host
            .admit_update("absent", "/nonexistent/update/source")
            .expect_err("a package that is not installed cannot be updated");
        assert!(
            matches!(err, PluginError::NotFound(ref name) if name == "absent"),
            "{err}"
        );
    }

    /// A source for a different package is refused before its component is
    /// read. As in the duplicate-name test, the candidate's component is a
    /// sparse file past the admission limit, so reading it would have produced
    /// the size-limit error instead.
    #[test]
    fn admit_update_refuses_a_source_for_another_package_before_reading_its_component() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        let source = tempdir().unwrap();
        write_tool_version(source.path(), "calendar", "2.0.0", b"");
        std::fs::File::options()
            .write(true)
            .open(source.path().join("plugin.wasm"))
            .unwrap()
            .set_len(MAX_COMPONENT_BYTES + 1)
            .unwrap();

        let err = host
            .admit_update("weather", source.path().to_str().unwrap())
            .expect_err("a source for another package must be refused");
        let message = err.to_string();
        assert!(matches!(err, PluginError::InvalidManifest(_)), "{message}");
        assert!(
            message.contains("'weather'") && message.contains("'calendar'"),
            "{message}"
        );
        assert!(!message.contains("admission limit"), "{message}");
    }

    /// Strict signature policy applies to a replacement exactly as to a new
    /// package: an unsigned candidate and one signed by an untrusted key are
    /// both refused and leave the installed package as it was, while one
    /// signed by a trusted key replaces it.
    #[test]
    fn admit_update_applies_signature_policy_and_keeps_the_installed_package() {
        let (trusted_key, trusted_public) = signature::generate_signing_key().unwrap();
        let (untrusted_key, untrusted_public) = signature::generate_signing_key().unwrap();
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir_with_security(
            plugins.path(),
            SignatureMode::Strict,
            vec![trusted_public.clone()],
        )
        .unwrap();
        let v1 = tempdir().unwrap();
        write_signed_tool_version(
            v1.path(),
            "signed-tool",
            "1.0.0",
            b"\0asm v1",
            &trusted_key,
            &trusted_public,
        );
        host.install(v1.path().to_str().unwrap()).unwrap();

        let unsigned = tempdir().unwrap();
        write_tool_version(unsigned.path(), "signed-tool", "2.0.0", b"\0asm v2");
        let err = host
            .admit_update("signed-tool", unsigned.path().to_str().unwrap())
            .expect_err("strict policy refuses an unsigned replacement");
        assert!(matches!(err, PluginError::UnsignedPlugin(_)), "{err}");

        let untrusted = tempdir().unwrap();
        write_signed_tool_version(
            untrusted.path(),
            "signed-tool",
            "2.0.0",
            b"\0asm v2",
            &untrusted_key,
            &untrusted_public,
        );
        let err = host
            .admit_update("signed-tool", untrusted.path().to_str().unwrap())
            .expect_err("strict policy refuses an untrusted publisher");
        assert!(
            matches!(err, PluginError::UntrustedPublisher { .. }),
            "{err}"
        );
        assert_eq!(host.get_plugin("signed-tool").unwrap().version, "1.0.0");
        assert_eq!(
            std::fs::read(plugins.path().join("signed-tool/plugin.wasm")).unwrap(),
            b"\0asm v1"
        );

        let trusted = tempdir().unwrap();
        write_signed_tool_version(
            trusted.path(),
            "signed-tool",
            "2.0.0",
            b"\0asm v2",
            &trusted_key,
            &trusted_public,
        );
        let admitted = host
            .admit_update("signed-tool", trusted.path().to_str().unwrap())
            .unwrap();
        host.update_admitted(admitted).unwrap();
        assert_eq!(host.get_plugin("signed-tool").unwrap().version, "2.0.0");
    }

    /// A replacement whose component does not match its declared digest is
    /// refused at admission.
    #[test]
    fn admit_update_rejects_a_component_that_does_not_match_its_digest() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        let source = tempdir().unwrap();
        std::fs::write(
            source.path().join("manifest.toml"),
            format!(
                "name = \"weather\"\nversion = \"2.0.0\"\nwasm_path = \"plugin.wasm\"\nwasm_sha256 = \"{}\"\ncapabilities = [\"tool\"]\n",
                signature::sha256_hex(b"\0asm expected")
            ),
        )
        .unwrap();
        std::fs::write(source.path().join("plugin.wasm"), b"\0asm tampered").unwrap();

        let err = host
            .admit_update("weather", source.path().to_str().unwrap())
            .expect_err("a digest mismatch must be refused");
        assert!(
            matches!(err, PluginError::PayloadDigestMismatch { .. }),
            "{err}"
        );
        assert_eq!(host.get_plugin("weather").unwrap().version, "1.0.0");
    }

    /// What was admitted is what replaces the package: a source changed after
    /// admission, the window in which the CLI runs its load check, does not
    /// change the installed bytes.
    #[test]
    fn update_installs_the_admitted_bytes_not_the_source_after_a_swap() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        let source = tempdir().unwrap();
        write_tool_version(source.path(), "weather", "2.0.0", b"\0asm admitted");
        let admitted = host
            .admit_update("weather", source.path().to_str().unwrap())
            .unwrap();
        std::fs::write(source.path().join("plugin.wasm"), b"\0asm swapped in").unwrap();
        host.update_admitted(admitted).unwrap();

        assert_eq!(
            std::fs::read(plugins.path().join("weather/plugin.wasm")).unwrap(),
            b"\0asm admitted"
        );
    }

    /// A component under `skills/` holds the admitted bytes as well: the copy
    /// of `skills/`, which reads the source as it is by then, never overwrites
    /// it.
    #[test]
    fn a_component_under_skills_keeps_the_admitted_bytes_after_a_source_swap() {
        fn write_skill_tool(dir: &Path, version: &str, wasm: &[u8]) {
            std::fs::write(
                dir.join("manifest.toml"),
                format!(
                    "name = \"kit\"\nversion = \"{version}\"\nwasm_path = \"skills/tool.wasm\"\ncapabilities = [\"tool\", \"skill\"]\n"
                ),
            )
            .unwrap();
            std::fs::create_dir_all(dir.join("skills/alpha")).unwrap();
            write_skill_md(&dir.join("skills/alpha/SKILL.md"), "alpha", "A skill");
            std::fs::write(dir.join("skills/tool.wasm"), wasm).unwrap();
        }
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let v1 = tempdir().unwrap();
        write_skill_tool(v1.path(), "1.0.0", b"\0asm v1");
        host.install(v1.path().to_str().unwrap()).unwrap();

        let v2 = tempdir().unwrap();
        write_skill_tool(v2.path(), "2.0.0", b"\0asm admitted");
        let admitted = host
            .admit_update("kit", v2.path().to_str().unwrap())
            .unwrap();
        std::fs::write(v2.path().join("skills/tool.wasm"), b"\0asm swapped in").unwrap();
        host.update_admitted(admitted).unwrap();

        assert_eq!(
            std::fs::read(plugins.path().join("kit/skills/tool.wasm")).unwrap(),
            b"\0asm admitted"
        );
        let rediscovered = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(
            rediscovered.admitted_component("kit").unwrap().bytes(),
            b"\0asm admitted"
        );
    }

    /// When the replacement cannot be published, the claimed generation is
    /// published back: the error is the publication's, the installed bytes and
    /// the host's view are unchanged, and nothing hidden is left behind.
    #[test]
    fn a_failed_publication_puts_the_claimed_generation_back() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        // Take the staged replacement away, so there is nothing to publish.
        replace_hook::set(|plugins, stage, _| {
            std::fs::remove_dir_all(plugins.join(stage).join(recovery::PACKAGE)).unwrap();
        });
        let err = update_tool(&mut host, "weather", "2.0.0", b"\0asm v2")
            .expect_err("the publication must fail");

        assert!(matches!(err, PluginError::Io(_)), "{err}");
        assert_eq!(host.get_plugin("weather").unwrap().version, "1.0.0");
        assert_eq!(
            std::fs::read(plugins.path().join("weather/plugin.wasm")).unwrap(),
            b"\0asm v1"
        );
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// When the claimed generation cannot be published back either, because
    /// something took its name, the update reports where it is preserved and
    /// overwrites nothing. Once the name is free, recovery puts it back.
    #[test]
    fn a_failed_put_back_reports_the_preserved_generation_and_recovery_restores_it() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let v1 = package_bytes(&plugins.path().join("weather"));

        replace_hook::set(|plugins, _, _| std::fs::create_dir(plugins.join("weather")).unwrap());
        let err = update_tool(&mut host, "weather", "2.0.0", b"\0asm v2")
            .expect_err("both publications must fail");

        let PluginError::ReplacementInterrupted { preserved, .. } = &err else {
            panic!("expected ReplacementInterrupted, got {err}");
        };
        assert_eq!(package_bytes(preserved), v1);
        assert!(host.get_plugin("weather").is_none());
        assert!(dir_entries(&plugins.path().join("weather")).is_empty());

        std::fs::remove_dir(plugins.path().join("weather")).unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(host.displaced_packages().unwrap(), ["weather"]);
        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Restored { kept: Vec::new() }
        );
        assert_eq!(package_bytes(&plugins.path().join("weather")), v1);
        assert_eq!(host.get_plugin("weather").unwrap().version, "1.0.0");
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// The generation an update claims must be the one the host loaded, whose
    /// authority the caller compared. A package replaced on disk since is
    /// published back untouched and the update refused.
    #[test]
    fn an_update_refuses_a_package_that_changed_since_this_host_loaded_it() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let source = tempdir().unwrap();
        write_tool_version(source.path(), "weather", "2.0.0", b"\0asm v2");
        let admitted = host
            .admit_update("weather", source.path().to_str().unwrap())
            .unwrap();

        // Another host removes the package and installs a different one.
        std::fs::remove_dir_all(plugins.path().join("weather")).unwrap();
        std::fs::create_dir(plugins.path().join("weather")).unwrap();
        write_tool_version(
            &plugins.path().join("weather"),
            "weather",
            "1.5.0",
            b"\0asm reinstalled",
        );
        let reinstalled = package_bytes(&plugins.path().join("weather"));

        let err = host
            .update_admitted(admitted)
            .expect_err("a changed package is not this update's to replace");
        assert!(matches!(err, PluginError::NamespaceChanged(_)), "{err}");
        assert_eq!(package_bytes(&plugins.path().join("weather")), reinstalled);
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// Recovery puts back the generation an update claimed when it stopped
    /// before publishing its replacement, and lists it until then.
    #[test]
    fn recovery_restores_the_generation_an_interrupted_update_claimed() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let v1 = package_bytes(&plugins.path().join("weather"));
        abandon_claim(&host, "weather", false);

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.get_plugin("weather").is_none());
        assert_eq!(host.displaced_packages().unwrap(), ["weather"]);
        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Restored { kept: Vec::new() }
        );
        assert_eq!(package_bytes(&plugins.path().join("weather")), v1);
        assert_eq!(host.get_plugin("weather").unwrap().version, "1.0.0");
        assert!(host.displaced_packages().unwrap().is_empty());
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// A batch's sweep never puts a displaced package back, even one this host
    /// still lists because the update that displaced it stopped after this
    /// host's discovery: it reports the claim and leaves it, and recovering
    /// the package by name then restores it.
    #[test]
    fn a_sweep_leaves_a_package_displaced_after_discovery_where_it_is() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let v1 = package_bytes(&plugins.path().join("weather"));
        let entry = abandon_claim(&host, "weather", false);
        assert!(host.get_plugin("weather").is_some());

        assert_eq!(
            host.sweep_interrupted_update("weather").unwrap(),
            UpdateRecovery::Displaced {
                displaced: vec![plugins.path().join(&entry).join(recovery::PACKAGE)],
                kept: Vec::new(),
            }
        );
        assert!(!plugins.path().join("weather").exists());
        assert!(host.get_plugin("weather").is_none());
        assert_eq!(host.displaced_packages().unwrap(), ["weather"]);

        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Restored { kept: Vec::new() }
        );
        assert_eq!(package_bytes(&plugins.path().join("weather")), v1);
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// A batch's sweep still deletes what finished updates left of a package
    /// that is installed.
    #[test]
    fn a_sweep_deletes_claimed_generations_of_an_installed_package() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        abandon_claim(&host, "weather", false);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "2.0.0", b"\0asm v2");

        assert_eq!(
            host.sweep_interrupted_update("weather").unwrap(),
            UpdateRecovery::Swept {
                removed: 1,
                kept: Vec::new()
            }
        );
        assert_eq!(host.get_plugin("weather").unwrap().version, "2.0.0");
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// Once the package is installed again, a generation an earlier update
    /// claimed is out of date, and recovery deletes it.
    #[test]
    fn recovery_deletes_claimed_generations_of_an_installed_package() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        abandon_claim(&host, "weather", false);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "2.0.0", b"\0asm v2");

        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Swept {
                removed: 1,
                kept: Vec::new()
            }
        );
        assert_eq!(host.get_plugin("weather").unwrap().version, "2.0.0");
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// A generation marked for deletion is never put back, even once the
    /// package is gone: recovery only deletes it, and it is never listed as
    /// displaced.
    #[test]
    fn recovery_never_puts_back_a_generation_marked_for_deletion() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        abandon_claim(&host, "weather", true);

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.displaced_packages().unwrap().is_empty());
        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Swept {
                removed: 1,
                kept: Vec::new()
            }
        );
        assert!(!plugins.path().join("weather").exists());
        assert!(host.get_plugin("weather").is_none());
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// With several claimed generations of a missing package, which was
    /// installed last is not known, so recovery moves none of them.
    #[test]
    fn recovery_refuses_to_choose_between_several_claimed_generations() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let first = abandon_claim(&host, "weather", false);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.5.0", b"\0asm v1.5");
        let second = abandon_claim(&host, "weather", false);

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let mut expected = [first, second].map(|entry| {
            PathBuf::from(
                plugins
                    .path()
                    .join(entry)
                    .join(recovery::PACKAGE)
                    .display()
                    .to_string(),
            )
        });
        expected.sort();
        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Ambiguous {
                displaced: expected.to_vec(),
                kept: Vec::new(),
            }
        );
        assert!(
            expected
                .iter()
                .all(|claim| claim.join("plugin.wasm").is_file())
        );
        assert!(!plugins.path().join("weather").exists());
    }

    /// A claimed generation is never published onto a name something else
    /// occupies, even an occupant this host cannot load.
    #[test]
    fn recovery_never_puts_a_claim_onto_an_occupied_name() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let claim = abandon_claim(&host, "weather", false);
        std::fs::create_dir(plugins.path().join("weather")).unwrap();
        std::fs::write(plugins.path().join("weather/manifest.toml"), "name =").unwrap();

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Occupied {
                displaced: PathBuf::from(
                    plugins
                        .path()
                        .join(&claim)
                        .join(recovery::PACKAGE)
                        .display()
                        .to_string()
                ),
                occupant: plugins.path().join("weather"),
                kept: Vec::new(),
            }
        );
        assert_eq!(
            std::fs::read_to_string(plugins.path().join("weather/manifest.toml")).unwrap(),
            "name ="
        );
        assert!(
            plugins
                .path()
                .join(&claim)
                .join(recovery::PACKAGE)
                .join("plugin.wasm")
                .is_file()
        );
    }

    /// Whether the package is installed is judged from the plugins directory
    /// under the lock, not from the view a host took when it was built. Here
    /// the host loaded the package, then another host's update claimed it and
    /// stopped: the claim is the only copy, and recovery puts it back.
    #[test]
    fn recovery_judges_installation_from_the_directory_not_a_stale_view() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let v1 = package_bytes(&plugins.path().join("weather"));
        let other = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        abandon_claim(&other, "weather", false);
        assert!(
            host.get_plugin("weather").is_some(),
            "premise: a stale view"
        );

        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Restored { kept: Vec::new() }
        );
        assert_eq!(package_bytes(&plugins.path().join("weather")), v1);
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// Following the advice for several claims, renaming the one to keep back
    /// to the package name, leaves the other claim out of date and the moved
    /// one empty: recovery deletes the first and finishes the second.
    #[test]
    fn recovery_finishes_a_claim_the_ambiguous_remedy_emptied() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let kept = abandon_claim(&host, "weather", false);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.5.0", b"\0asm v1.5");
        abandon_claim(&host, "weather", false);
        std::fs::rename(
            plugins.path().join(&kept).join(recovery::PACKAGE),
            plugins.path().join("weather"),
        )
        .unwrap();

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Swept {
                removed: 1,
                kept: Vec::new()
            }
        );
        assert_eq!(host.get_plugin("weather").unwrap().version, "1.0.0");
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// A process stopped while creating or finishing a transaction leaves an
    /// empty directory without a lease, which recovery removes. One that is
    /// not empty is never judged by its name alone: it is reported.
    #[test]
    fn recovery_removes_an_empty_transaction_and_reports_one_that_is_not() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let suffix = "0".repeat(32);
        std::fs::create_dir(
            plugins
                .path()
                .join(format!(".weather.replacing-v1-{suffix}")),
        )
        .unwrap();

        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Nothing
        );
        assert!(hidden_entries(plugins.path()).is_empty());

        let unleased = plugins
            .path()
            .join(format!(".weather.replacing-v1-{suffix}"));
        std::fs::create_dir_all(unleased.join(recovery::PACKAGE)).unwrap();
        let err = host
            .recover_interrupted_update("weather")
            .expect_err("a non-empty transaction without a lease is not judged");
        assert!(matches!(err, PluginError::RecoveryRetained { .. }), "{err}");
        assert!(unleased.join(recovery::PACKAGE).is_dir());
    }

    /// A claim whose lease is still held belongs to an update that has not
    /// finished. Recovery reports it and touches nothing, and it is not listed
    /// as displaced.
    #[test]
    fn recovery_leaves_a_claim_whose_lease_is_held() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let v1 = package_bytes(&plugins.path().join("weather"));
        let root = host.root().unwrap();
        let running = root.transaction("weather", "replacing").unwrap();
        running.claim(root, "weather").unwrap();

        let mut other = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(other.displaced_packages().unwrap().is_empty());
        let err = other
            .recover_interrupted_update("weather")
            .expect_err("a held lease is never judged");
        assert!(matches!(err, PluginError::RecoveryRetained { .. }), "{err}");
        assert_eq!(
            package_bytes(&plugins.path().join(&running.entry).join(recovery::PACKAGE)),
            v1
        );
        assert!(!plugins.path().join("weather").exists());
        drop(running);
    }

    /// Only `replacing` transactions of the package itself are claims: a
    /// hidden directory with a near-miss name, or another package's claim, is
    /// left alone.
    #[test]
    fn only_replacing_transactions_of_the_package_are_claims() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "calendar", "1.0.0", b"\0asm calendar");
        let other = abandon_claim(&host, "calendar", false);
        for decoy in [
            ".weather.replacing-v1-not-hex",
            ".weather.replaced-4242",
            ".weather.replacing-v2-00000000000000000000000000000000",
        ] {
            std::fs::create_dir_all(plugins.path().join(decoy).join(recovery::PACKAGE)).unwrap();
        }

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(host.displaced_packages().unwrap(), ["calendar"]);
        assert_eq!(
            host.recover_interrupted_update("weather").unwrap(),
            UpdateRecovery::Nothing
        );
        assert_eq!(hidden_entries(plugins.path()).len(), 4);
        assert!(plugins.path().join(&other).join(recovery::PACKAGE).is_dir());
    }

    /// A skill bundle is replaced as a whole: the new bundle's skills are
    /// installed and a skill only the old bundle had is gone.
    #[test]
    fn update_replaces_a_skill_bundle_as_a_whole() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let v1 = tempdir().unwrap();
        write_skill_bundle_plugin(v1.path(), "toolkit", &["alpha"]);
        host.install(v1.path().join("toolkit").to_str().unwrap())
            .unwrap();

        let v2 = tempdir().unwrap();
        write_skill_bundle_plugin(v2.path(), "toolkit", &["beta"]);
        let manifest = v2.path().join("toolkit/manifest.toml");
        let text = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("0.1.0", "0.2.0");
        std::fs::write(&manifest, text).unwrap();
        let admitted = host
            .admit_update("toolkit", v2.path().join("toolkit").to_str().unwrap())
            .unwrap();
        host.update_admitted(admitted).unwrap();

        assert!(
            plugins
                .path()
                .join("toolkit/skills/beta/SKILL.md")
                .is_file()
        );
        assert!(!plugins.path().join("toolkit/skills/alpha").exists());
        assert_eq!(host.get_plugin("toolkit").unwrap().version, "0.2.0");
    }

    /// A skill bundle at 0.2.0 whose one skill `beta` replaces `alpha`, with
    /// the skill's `SKILL.md` or the whole skill directory a symbolic link.
    #[cfg(unix)]
    fn linked_skill_bundle(link_directory: bool) -> tempfile::TempDir {
        use std::os::unix::fs::symlink;

        let v2 = tempdir().unwrap();
        write_skill_bundle_plugin(v2.path(), "toolkit", &["beta"]);
        let source = v2.path().join("toolkit");
        let manifest = source.join("manifest.toml");
        let text = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("0.1.0", "0.2.0");
        std::fs::write(&manifest, text).unwrap();
        // Relative targets inside the package: admission reads through a
        // handle on the package and never follows a link out of it.
        let (link, target, relative) = if link_directory {
            (
                source.join("skills/beta"),
                source.join("skill-target"),
                "../skill-target",
            )
        } else {
            (
                source.join("skills/beta/SKILL.md"),
                source.join("skill-target.md"),
                "../../skill-target.md",
            )
        };
        std::fs::rename(&link, &target).unwrap();
        symlink(relative, &link).unwrap();
        v2
    }

    /// Admission follows a linked `SKILL.md` inside the package, but staging
    /// copies no links, so the staged bundle lacks it. The update is refused
    /// after staging, before the installed bundle is touched, saying why.
    #[cfg(unix)]
    #[test]
    fn update_refuses_a_staged_bundle_that_lost_a_linked_skill_md() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let v1 = tempdir().unwrap();
        write_skill_bundle_plugin(v1.path(), "toolkit", &["alpha"]);
        host.install(v1.path().join("toolkit").to_str().unwrap())
            .unwrap();
        let installed = plugins.path().join("toolkit");
        let skill = std::fs::read(installed.join("skills/alpha/SKILL.md")).unwrap();

        let v2 = linked_skill_bundle(false);
        let admitted = host
            .admit_update("toolkit", v2.path().join("toolkit").to_str().unwrap())
            .expect("admission follows a link inside the package");
        let err = host
            .update_admitted(admitted)
            .expect_err("the staged bundle lacks the linked SKILL.md");

        let message = err.to_string();
        assert!(matches!(err, PluginError::InvalidManifest(_)), "{message}");
        assert!(
            message.contains("staged copy of 'toolkit' is incomplete")
                && message.contains("subdirectory 'beta' is missing SKILL.md")
                && message.contains("symbolic links"),
            "{message}"
        );
        assert_eq!(
            std::fs::read(installed.join("skills/alpha/SKILL.md")).unwrap(),
            skill
        );
        assert!(!installed.join("skills/beta").exists());
        assert_eq!(host.get_plugin("toolkit").unwrap().version, "0.1.0");
        assert!(hidden_entries(plugins.path()).is_empty());
        let fresh = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(fresh.get_plugin("toolkit").unwrap().version, "0.1.0");
    }

    /// Install refuses the same incomplete staged bundle an update refuses,
    /// instead of publishing a package discovery would then skip.
    #[cfg(unix)]
    #[test]
    fn install_refuses_a_staged_bundle_that_lost_a_linked_skill_md() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        let v2 = linked_skill_bundle(false);
        let err = host
            .install(v2.path().join("toolkit").to_str().unwrap())
            .expect_err("the staged bundle lacks the linked SKILL.md");

        assert!(
            err.to_string()
                .contains("staged copy of 'toolkit' is incomplete"),
            "{err}"
        );
        assert!(!plugins.path().join("toolkit").exists());
        assert!(host.get_plugin("toolkit").is_none());
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// A skill directory that is itself a link is not a skill to admission, so
    /// a bundle whose only skill is one is refused before anything is staged.
    #[cfg(unix)]
    #[test]
    fn admit_update_refuses_a_bundle_whose_only_skill_is_a_linked_directory() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let v1 = tempdir().unwrap();
        write_skill_bundle_plugin(v1.path(), "toolkit", &["alpha"]);
        host.install(v1.path().join("toolkit").to_str().unwrap())
            .unwrap();

        let v2 = linked_skill_bundle(true);
        let err = host
            .admit_update("toolkit", v2.path().join("toolkit").to_str().unwrap())
            .expect_err("a linked skill directory is not a skill");
        assert!(
            err.to_string().contains("empty `skills/` directory"),
            "{err}"
        );
        assert_eq!(host.get_plugin("toolkit").unwrap().version, "0.1.0");
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// A package discovered in a directory named differently from its manifest
    /// is replaced in that directory, so discovery never sees two packages of
    /// one name afterwards.
    #[test]
    fn update_keeps_the_directory_the_package_was_discovered_in() {
        let plugins = tempdir().unwrap();
        let discovered_in = plugins.path().join("legacy-dir");
        std::fs::create_dir(&discovered_in).unwrap();
        write_tool_version(&discovered_in, "weather", "1.0.0", b"\0asm v1");
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        update_tool(&mut host, "weather", "2.0.0", b"\0asm v2").unwrap();

        assert_eq!(
            std::fs::read(discovered_in.join("plugin.wasm")).unwrap(),
            b"\0asm v2"
        );
        assert!(!plugins.path().join("weather").exists());
        let rediscovered = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert_eq!(rediscovered.get_plugin("weather").unwrap().version, "2.0.0");
    }

    /// A package discovered in a directory named differently from its manifest
    /// is removed from that directory. An unrelated directory at its name is
    /// not the package's, and is left as it is.
    #[test]
    fn remove_deletes_the_directory_the_package_was_loaded_from() {
        let plugins = tempdir().unwrap();
        let discovered_in = plugins.path().join("legacy-dir");
        std::fs::create_dir(&discovered_in).unwrap();
        write_tool_version(&discovered_in, "weather", "1.0.0", b"\0asm v1");
        let unrelated = plugins.path().join("weather");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("notes.txt"), b"keep").unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.get_plugin("weather").is_some());

        host.remove("weather").unwrap();

        assert!(!discovered_in.exists(), "the loaded package is deleted");
        assert_eq!(
            package_bytes(&unrelated),
            vec![("notes.txt".to_string(), b"keep".to_vec())],
            "the directory at the package's name is not the package's"
        );
        assert!(host.get_plugin("weather").is_none());
        assert!(hidden_entries(plugins.path()).is_empty());
        let rediscovered = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(rediscovered.get_plugin("weather").is_none());
    }

    /// A loaded package whose directory is already gone leaves nothing to
    /// delete, and its name is not a fallback: what sits there is kept.
    #[test]
    fn remove_of_a_package_whose_directory_is_gone_keeps_what_is_at_its_name() {
        let plugins = tempdir().unwrap();
        let discovered_in = plugins.path().join("legacy-dir");
        std::fs::create_dir(&discovered_in).unwrap();
        write_tool_version(&discovered_in, "weather", "1.0.0", b"\0asm v1");
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        std::fs::remove_dir_all(&discovered_in).unwrap();
        let unrelated = plugins.path().join("weather");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("notes.txt"), b"keep").unwrap();

        host.remove("weather").unwrap();

        assert_eq!(
            package_bytes(&unrelated),
            vec![("notes.txt".to_string(), b"keep".to_vec())]
        );
        assert!(host.get_plugin("weather").is_none());
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// Remove deletes only the generation this host loaded. A package
    /// replaced on disk since is put back untouched and the remove refused.
    #[test]
    fn remove_refuses_a_package_that_changed_since_this_host_loaded_it() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        // Another host removes the package and installs a different one.
        std::fs::remove_dir_all(plugins.path().join("weather")).unwrap();
        std::fs::create_dir(plugins.path().join("weather")).unwrap();
        write_tool_version(
            &plugins.path().join("weather"),
            "weather",
            "1.5.0",
            b"\0asm reinstalled",
        );
        let reinstalled = package_bytes(&plugins.path().join("weather"));

        let err = host
            .remove("weather")
            .expect_err("a changed package is not this remove's to delete");
        assert!(matches!(err, PluginError::NamespaceChanged(_)), "{err}");
        assert_eq!(package_bytes(&plugins.path().join("weather")), reinstalled);
        assert!(hidden_entries(plugins.path()).is_empty());
        assert!(
            host.get_plugin("weather").is_some(),
            "a refused remove keeps the package loaded"
        );
    }

    /// An unrelated directory swapped in for a loaded package after load is
    /// not the loaded generation: it is put back as it was, and the remove is
    /// refused as a namespace change.
    #[test]
    fn remove_puts_back_an_unrelated_directory_swapped_in_for_a_loaded_package() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let installed = plugins.path().join("weather");
        std::fs::remove_dir_all(&installed).unwrap();
        std::fs::create_dir(&installed).unwrap();
        std::fs::write(installed.join("notes.txt"), b"keep").unwrap();

        let err = host
            .remove("weather")
            .expect_err("an unrelated directory is not this remove's to delete");
        assert!(matches!(err, PluginError::NamespaceChanged(_)), "{err}");
        assert_eq!(
            package_bytes(&installed),
            vec![("notes.txt".to_string(), b"keep".to_vec())]
        );
        assert!(hidden_entries(plugins.path()).is_empty());
        assert!(host.get_plugin("weather").is_some());
    }

    /// A symlink swapped in for a loaded package after load, even to an
    /// identical copy outside the plugins directory, is never followed: the
    /// link is put back, its target is untouched, and the remove is refused.
    #[cfg(unix)]
    #[test]
    fn remove_puts_back_a_symlink_swapped_in_for_a_loaded_package() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let outside = tempdir().unwrap();
        let copy = outside.path().join("weather");
        std::fs::create_dir(&copy).unwrap();
        write_tool_version(&copy, "weather", "1.0.0", b"\0asm v1");
        let target = package_bytes(&copy);
        let installed = plugins.path().join("weather");
        std::fs::remove_dir_all(&installed).unwrap();
        std::os::unix::fs::symlink(&copy, &installed).unwrap();

        let err = host
            .remove("weather")
            .expect_err("a symlink is not this remove's to follow");
        assert!(matches!(err, PluginError::Io(_)), "{err}");
        assert!(
            std::fs::symlink_metadata(&installed)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(package_bytes(&copy), target);
        assert!(hidden_entries(plugins.path()).is_empty());
        assert!(host.get_plugin("weather").is_some());
    }

    /// A loaded skill bundle, which ships no component, is removed from the
    /// directory it was loaded from like any other package.
    #[test]
    fn remove_deletes_a_loaded_skill_bundle_from_the_directory_it_was_loaded_from() {
        let plugins = tempdir().unwrap();
        write_skill_bundle_plugin(plugins.path(), "greetings", &["greet"]);
        let discovered_in = plugins.path().join("legacy-dir");
        std::fs::rename(plugins.path().join("greetings"), &discovered_in).unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.get_plugin("greetings").is_some());

        host.remove("greetings").unwrap();

        assert!(!discovered_in.exists());
        assert!(host.get_plugin("greetings").is_none());
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// Where the no-replace move is unsupported, removing a loaded package
    /// refuses with the move's error and keeps the package installed and
    /// loaded.
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn remove_of_a_loaded_package_refuses_where_the_no_replace_move_is_unsupported() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let installed = package_bytes(&plugins.path().join("weather"));

        let previous = recovery::FORCE_RENAME_ERROR
            .with(|error| error.replace(Some(rustix::io::Errno::NOSYS)));
        let err = host.remove("weather");
        recovery::FORCE_RENAME_ERROR.with(|error| error.set(previous));

        let err = err.expect_err("no claim without a no-replace move");
        assert!(
            matches!(&err, PluginError::Io(error) if error.kind() == std::io::ErrorKind::Unsupported),
            "{err}"
        );
        assert!(
            err.to_string()
                .starts_with("IO error: atomic no-replace package move unavailable"),
            "{err}"
        );
        assert_eq!(package_bytes(&plugins.path().join("weather")), installed);
        assert!(host.get_plugin("weather").is_some());
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// A loaded package whose claimed directory cannot be deleted is reported
    /// where it is kept, marked for deletion, and the next remove finishes the
    /// delete.
    #[cfg(unix)]
    #[test]
    fn a_loaded_package_that_cannot_be_deleted_is_reported_and_the_next_remove_finishes_it() {
        if !permissions_can_block_a_delete() {
            return;
        }
        let plugins = tempdir().unwrap();
        let discovered_in = plugins.path().join("legacy-dir");
        std::fs::create_dir(&discovered_in).unwrap();
        write_tool_version(&discovered_in, "weather", "1.0.0", b"\0asm v1");
        lock_a_file_inside(&discovered_in);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.get_plugin("weather").is_some());

        let err = host
            .remove("weather")
            .expect_err("the locked file cannot be deleted");
        let claim = hidden_entries(plugins.path())
            .into_iter()
            .find(|entry| entry.contains("recovering-v1"))
            .expect("the claim is kept");
        assert!(
            matches!(&err, PluginError::RecoveryRetained { path, .. } if path.contains(&claim)),
            "{err}"
        );
        assert!(!discovered_in.exists());
        assert!(host.get_plugin("weather").is_none());

        unlock(&plugins.path().join(&claim).join("package"));
        PluginHost::from_plugins_dir(plugins.path())
            .unwrap()
            .remove("weather")
            .expect("the next remove finishes the delete");
        assert!(hidden_entries(plugins.path()).is_empty());
        assert!(!plugins.path().join("weather").exists());
    }

    /// A replacement that cannot be staged never touches the installed
    /// package, and its partial stage is deleted.
    #[test]
    fn a_failed_staging_write_leaves_the_installed_package_untouched() {
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let v1 = package_bytes(&plugins.path().join("weather"));

        let armed = write_fault::arm(write_fault::Step::Payload);
        let err = update_tool(&mut host, "weather", "2.0.0", b"\0asm v2")
            .expect_err("the staged payload write fails");
        drop(armed);

        assert!(err.to_string().contains("injected"), "{err}");
        assert_eq!(package_bytes(&plugins.path().join("weather")), v1);
        assert_eq!(host.get_plugin("weather").unwrap().version, "1.0.0");
        assert!(hidden_entries(plugins.path()).is_empty());
    }

    /// Whether this process can be kept from deleting a file by permissions:
    /// not when it runs as root.
    #[cfg(unix)]
    fn permissions_can_block_a_delete() -> bool {
        use std::os::unix::fs::PermissionsExt;

        let probe = tempdir().unwrap();
        std::fs::write(probe.path().join("file"), b"x").unwrap();
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let deletable = std::fs::remove_file(probe.path().join("file")).is_ok();
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        !deletable
    }

    /// Put a file `dir` cannot lose inside `dir/locked`, so deleting `dir`
    /// fails. [`unlock`] undoes it so the temp dir can be cleaned up.
    #[cfg(unix)]
    fn lock_a_file_inside(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let locked = dir.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("file"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    }

    #[cfg(unix)]
    fn unlock(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    /// A replaced generation that cannot be deleted stays in its transaction,
    /// marked for deletion, and is reported there. After the package is
    /// removed, recovery still never puts it back.
    #[cfg(unix)]
    #[test]
    fn a_replaced_generation_that_cannot_be_deleted_is_kept_marked() {
        if !permissions_can_block_a_delete() {
            return;
        }
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");

        replace_hook::set(|plugins, _, claim| {
            lock_a_file_inside(&plugins.join(claim).join(recovery::PACKAGE));
        });
        let replaced = update_tool(&mut host, "weather", "2.0.0", b"\0asm v2").unwrap();
        let (left, _) = replaced.leftover.expect("the replaced generation stays");
        assert!(left.ends_with(recovery::PACKAGE), "{}", left.display());
        assert!(left.parent().unwrap().join(recovery::DELETING).is_file());
        assert_eq!(host.get_plugin("weather").unwrap().version, "2.0.0");

        host.remove("weather").unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        assert!(host.displaced_packages().unwrap().is_empty());
        let recovered = host.recover_interrupted_update("weather").unwrap();
        assert!(
            matches!(&recovered, UpdateRecovery::Swept { removed: 0, kept } if kept.len() == 1),
            "{recovered:?}"
        );
        assert!(!plugins.path().join("weather").exists());
        unlock(&left);
    }

    /// Recovery that puts a package back still reports a generation marked for
    /// deletion that it could not delete, so the operator learns which hidden
    /// copy is left.
    #[cfg(unix)]
    #[test]
    fn recovery_that_restores_a_package_reports_a_generation_it_cannot_delete() {
        if !permissions_can_block_a_delete() {
            return;
        }
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let old = abandon_claim(&host, "weather", true);
        let left = plugins.path().join(&old).join(recovery::PACKAGE);
        lock_a_file_inside(&left);
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "2.0.0", b"\0asm v2");
        let v2 = package_bytes(&plugins.path().join("weather"));
        abandon_claim(&host, "weather", false);

        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        let recovered = host.recover_interrupted_update("weather").unwrap();
        assert!(
            matches!(&recovered, UpdateRecovery::Restored { kept } if kept.len() == 1 && kept[0].0 == left),
            "{recovered:?}"
        );
        assert_eq!(recovered.kept().len(), 1);
        assert_eq!(package_bytes(&plugins.path().join("weather")), v2);
        assert_eq!(host.get_plugin("weather").unwrap().version, "2.0.0");
        unlock(&left);
    }

    /// A claimed generation recovery cannot delete is marked for deletion
    /// first, so it is reported, and left, where it can never be put back.
    #[cfg(unix)]
    #[test]
    fn recovery_reports_a_claimed_generation_it_cannot_delete() {
        if !permissions_can_block_a_delete() {
            return;
        }
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "1.0.0", b"\0asm v1");
        let claim = abandon_claim(&host, "weather", false);
        lock_a_file_inside(&plugins.path().join(&claim).join(recovery::PACKAGE));
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();
        install_tool_version(&mut host, "weather", "2.0.0", b"\0asm v2");

        let recovered = host.recover_interrupted_update("weather").unwrap();
        let left = plugins.path().join(&claim).join(recovery::PACKAGE);
        assert!(
            matches!(&recovered, UpdateRecovery::Swept { removed: 0, kept } if kept.len() == 1 && kept[0].0 == left),
            "{recovered:?}"
        );
        assert!(
            plugins
                .path()
                .join(&claim)
                .join(recovery::DELETING)
                .is_file()
        );
        assert_eq!(host.get_plugin("weather").unwrap().version, "2.0.0");
        unlock(&left);
    }
}
