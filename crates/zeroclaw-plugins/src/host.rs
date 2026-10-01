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
    recovery_root: recovery::Root,
    loaded: HashMap<String, LoadedPlugin>,
    signature_mode: SignatureMode,
    trusted_publisher_keys: Vec<String>,
}

struct LoadedPlugin {
    manifest: PluginManifest,
    /// The exact manifest text admitted with this generation, so an update
    /// can tell whether the package it claims is still the one loaded here.
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
    /// discovery ignores, marked superseded, so recovery only ever deletes it.
    /// If even that marking failed, it is still a claimed generation, which
    /// recovery deletes while the package is installed but would put back once
    /// the package is removed, so it is best deleted by hand.
    pub leftover: Option<(PathBuf, String)>,
}

/// What [`PluginHost::recover_interrupted_update`] found for one package.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateRecovery {
    /// There was nothing to put back or delete. Abandoned stages and empty
    /// transactions may have been cleared.
    Nothing,
    /// Out-of-date or superseded generations earlier updates claimed were
    /// found. `removed` were deleted. `kept` could not be, each with where it
    /// was left and why: a generation marked superseded is never loaded or put
    /// back, while one whose marking failed is still a claim.
    Swept {
        removed: usize,
        kept: Vec<(PathBuf, String)>,
    },
    /// The package was missing, and the one generation an interrupted update
    /// claimed was put back and discovery ran again. The configured policy
    /// decides whether it is admitted; [`PluginHost::get_plugin`] says whether
    /// it was.
    Restored,
    /// The package is missing and more than one interrupted update claimed a
    /// generation of it, so which was installed last is not known. Nothing
    /// was moved.
    Ambiguous { displaced: Vec<PathBuf> },
    /// The package is not loaded, but something already occupies its name, so
    /// the claimed generation was not moved onto it.
    Occupied {
        displaced: PathBuf,
        occupant: PathBuf,
    },
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
            recovery_root: recovery::Root::open(plugins_dir)?,
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
        self.loaded = self.discovered()?;
        Ok(())
    }

    /// The packages discovery admits from the plugins directory as it is now.
    fn discovered(&self) -> Result<HashMap<String, LoadedPlugin>, PluginError> {
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
                            let component = match admit_component(&path, &manifest) {
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

        let source_dir = manifest_path
            .parent()
            .ok_or_else(|| PluginError::InvalidManifest("no parent directory".into()))?
            .to_path_buf();
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

        let _guard = self.recovery_root.lock()?;
        let dest_dir = self.plugins_dir.join(&manifest.name);
        match self.recovery_root.dir.symlink_metadata(&manifest.name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(PluginError::UnadmittedPackage {
                    name: manifest.name.clone(),
                    reason: if self
                        .recovery_root
                        .dir
                        .symlink_metadata(&manifest.name)?
                        .is_dir()
                    {
                        "an existing directory occupies the destination".into()
                    } else {
                        "it is not a directory".into()
                    },
                });
            }
        }
        let tx = self
            .recovery_root
            .transaction(&manifest.name, "installing")?;
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
            recovery::clear_owned(&package)?;
            drop(package);
            tx.dir.remove_dir(recovery::PACKAGE)?;
            tx.finish(&self.recovery_root)?;
            return Err(error);
        }
        drop(package);
        // Atomic no-clobber publication, even against an unrelated writer.
        if let Err(error) = tx.publish(&self.recovery_root, &manifest.name) {
            return Err(PluginError::RecoveryRetained {
                path: self.recovery_root.retained_path(&tx),
                reason: error.to_string(),
            });
        }
        tx.finish(&self.recovery_root)?;

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
    /// For a loaded plugin, the directory at its name is deleted. For a name the
    /// host did not load, `remove` is the recovery path for an interrupted
    /// install: it claims and validates the exact directory generation and
    /// cleans only staging whose protocol lease proves abandonment. Ambiguous
    /// legacy staging is retained. The final directory must be empty, or
    /// holds a `manifest.toml` and admission rejects its own contents rather
    /// than its signature. That includes a package this host cannot accept as
    /// written, such as one whose component exceeds the admission size limit.
    /// Anything else at the name is left untouched and reported as
    /// [`PluginError::UnadmittedPackage`] with the reason: a symlink or file, a
    /// directory holding files but no manifest, one that cannot be identified,
    /// inspected, or listed, a loaded package's directory, a package admission
    /// accepts, and one this host rejects for its signature policy. A refusal
    /// restores the claimed package without overwriting a concurrent occupant;
    /// if restoration cannot complete, `RecoveryRetained` names the retained
    /// transaction and a later remove retries it.
    pub fn remove(&mut self, name: &str) -> Result<(), PluginError> {
        self.remove_with_report(name).map(|_| ())
    }

    /// Removal result includes ambiguous staging paths that were deliberately
    /// retained. Callers displaying a recovery result must surface these paths.
    pub fn remove_with_report(&mut self, name: &str) -> Result<Vec<PathBuf>, PluginError> {
        let _guard = self.recovery_root.lock()?;
        if self.loaded.remove(name).is_some() {
            // Existing loaded-package semantics are not the recovery classifier.
            let plugin_dir = self.plugins_dir.join(name);
            if plugin_dir.exists() {
                std::fs::remove_dir_all(plugin_dir)?;
            }
            return Ok(Vec::new());
        }
        crate::instance::validate_package_name(name)
            .map_err(|_| PluginError::NotFound(name.into()))?;
        self.retry_claims(name)?;
        let metadata = match std::fs::symlink_metadata(self.plugins_dir.join(name)) {
            Ok(metadata) => metadata,
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
        let tx = self.recovery_root.transaction(name, "recovering")?;
        #[cfg(test)]
        recovery::pause("claim-created");
        tx.claim(&self.recovery_root, name)?;
        #[cfg(test)]
        recovery::pause("after-claim");
        let claimed = match tx.dir.open_dir(recovery::PACKAGE) {
            Ok(dir) => dir,
            Err(error) => {
                return self.restore_refused(
                    name,
                    tx,
                    PluginError::InvalidConfig(format!(
                        "it cannot be listed or cannot be inspected ({error})"
                    )),
                );
            }
        };
        let claimed_id = same_file::Handle::from_file(claimed.try_clone()?.into_std_file())?;
        let same_generation = selected_id == claimed_id;
        drop(claimed_id);
        drop(selected_id);
        if !same_generation {
            drop(claimed);
            return self.restore_refused(name, tx, PluginError::NamespaceChanged(name.into()));
        }
        if let Err(error) = self.recovery_verdict(&claimed) {
            drop(claimed);
            return self.restore_refused(name, tx, error);
        }
        // Stages are handled only after a real final generation was claimed and
        // judged recoverable. No-final calls never sweep unrelated stages.
        let retained =
            self.remove_stale_staging(name)
                .map_err(|error| PluginError::RecoveryRetained {
                    path: self.recovery_root.retained_path(&tx),
                    reason: error.to_string(),
                })?;
        #[cfg(test)]
        recovery::pause("before-delete");
        self.recovery_root
            .check()
            .map_err(|error| PluginError::RecoveryRetained {
                path: self.recovery_root.retained_path(&tx),
                reason: error.to_string(),
            })?;
        // Admission is about these bytes, not a verdict retained across stage IO.
        let empty = match self.recovery_verdict(&claimed) {
            Ok(empty) => empty,
            Err(error) => {
                drop(claimed);
                return self.restore_refused(name, tx, error);
            }
        };
        if !empty {
            recovery::clear_owned(&claimed).map_err(|error| PluginError::RecoveryRetained {
                path: self.recovery_root.retained_path(&tx),
                reason: error.to_string(),
            })?;
        }
        drop(claimed);
        tx.dir
            .remove_dir(recovery::PACKAGE)
            .map_err(|error| PluginError::RecoveryRetained {
                path: self.recovery_root.retained_path(&tx),
                reason: error.to_string(),
            })?;
        tx.finish(&self.recovery_root)?;
        Ok(retained)
    }

    fn recovery_verdict(&self, dir: &Dir) -> Result<bool, PluginError> {
        match dir.symlink_metadata("manifest.toml") {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if dir.entries()?.next().transpose()?.is_none() {
                    return Ok(true);
                }
                return Err(PluginError::InvalidConfig(
                    "it holds files but no manifest.toml, so no interrupted install left it".into(),
                ));
            }
            Err(error) => {
                return Err(PluginError::InvalidConfig(format!(
                    "its manifest.toml cannot be inspected ({error})"
                )));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(PluginError::InvalidConfig(
                    "its manifest.toml is not a regular file".into(),
                ));
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
            Ok(_) => Err(PluginError::InvalidConfig(
                "admission accepts this package".into(),
            )),
            Err(error) if is_structural_admission_failure(&error) => {
                dir.entries().map_err(|error| {
                    PluginError::InvalidConfig(format!("it cannot be listed ({error})"))
                })?;
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
        #[cfg(test)]
        recovery::pause("before-restore");
        if let Err(error) = tx.publish(&self.recovery_root, name) {
            return Err(PluginError::RecoveryRetained {
                path: self.recovery_root.retained_path(&tx),
                reason: format!("{reason}; restoration refused: {error}"),
            });
        }
        tx.finish(&self.recovery_root)?;
        Err(PluginError::UnadmittedPackage {
            name: name.into(),
            reason: reason.to_string(),
        })
    }

    fn retry_claims(&self, name: &str) -> Result<(), PluginError> {
        for entry in self.recovery_root.dir.entries()? {
            let entry = entry?;
            let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !recovery::is_transaction(&entry_name, name, "recovering") {
                continue;
            }
            let Some(tx) = self.recovery_root.reopen_transaction(entry_name.clone())? else {
                return Err(PluginError::RecoveryRetained {
                    path: self.plugins_dir.join(entry_name).display().to_string(),
                    reason: "recovery transaction ownership is unavailable".into(),
                });
            };
            match tx.dir.symlink_metadata(recovery::PACKAGE) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tx.finish(&self.recovery_root)?;
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    // Restore before restarting recovery; crash retry never
                    // interprets hidden placement as proof of defective contents.
                    if let Err(error) = tx.publish(&self.recovery_root, name) {
                        return Err(PluginError::RecoveryRetained {
                            path: self.recovery_root.retained_path(&tx),
                            reason: error.to_string(),
                        });
                    }
                    tx.finish(&self.recovery_root)?;
                }
            }
        }
        Ok(())
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
        let mut retained = Vec::new();
        for entry in self.recovery_root.dir.entries()? {
            let entry = entry?;
            let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !entry_name.starts_with(&staging_prefix(name)) {
                continue;
            }
            if !recovery::is_transaction(&entry_name, name, "installing") {
                if entry_name
                    .strip_prefix(&staging_prefix(name))
                    .is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                    })
                {
                    retained.push(self.plugins_dir.join(entry_name));
                }
                continue;
            }
            let Some(tx) = self.recovery_root.reopen_transaction(entry_name.clone())? else {
                retained.push(self.plugins_dir.join(entry_name));
                continue;
            };
            // A protocol stage's held lease is the ownership fact. Never create
            // a lease in an unknown directory and call that proof of abandonment.
            match tx.dir.open_dir(recovery::PACKAGE) {
                Ok(package) => {
                    #[cfg(test)]
                    recovery::pause("before-stage-delete");
                    self.recovery_root.check()?;
                    recovery::clear_owned(&package)?;
                    drop(package);
                    tx.dir.remove_dir(recovery::PACKAGE)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            tx.finish(&self.recovery_root)?;
        }
        Ok(retained)
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
    /// it loaded. The
    /// replacement is then published in the directory the installed package
    /// was discovered in, and if that fails the claimed generation is
    /// published back. So on every error except
    /// [`PluginError::ReplacementInterrupted`] the installed package is exactly
    /// as it was.
    ///
    /// Configuration and durable state are not touched: they belong to the
    /// instance identity, which does not include the version.
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

        let _guard = self.recovery_root.lock()?;
        let stage = self
            .recovery_root
            .transaction(&manifest.name, "installing")?;
        if let Err(error) = stage_replacement(
            &stage,
            &manifest,
            &manifest_toml,
            &source_dir,
            component.as_ref(),
        ) {
            self.discard(stage);
            return Err(error);
        }
        #[cfg(test)]
        recovery::pause("update-staged");

        let held = match self.recovery_root.transaction(&manifest.name, "replacing") {
            Ok(held) => held,
            Err(error) => {
                self.discard(stage);
                return Err(error);
            }
        };
        #[cfg(test)]
        recovery::pause("update-before-claim");
        if let Err(error) = held.claim(&self.recovery_root, &entry) {
            let _ = held.finish(&self.recovery_root);
            self.discard(stage);
            return Err(error);
        }
        #[cfg(test)]
        recovery::pause("update-claimed");
        if let Err(error) = self.claimed_is_loaded(&held, &manifest.name) {
            self.discard(stage);
            let refused = self.put_back(&manifest.name, &entry, held, error);
            if matches!(refused, PluginError::ReplacementInterrupted { .. }) {
                self.loaded.remove(&manifest.name);
            }
            return Err(refused);
        }

        #[cfg(test)]
        replace_hook::run(&self.plugins_dir, &stage.entry, &held.entry);
        if let Err(error) = stage.publish(&self.recovery_root, &entry) {
            self.discard(stage);
            let refused = self.put_back(&manifest.name, &entry, held, error);
            if matches!(refused, PluginError::ReplacementInterrupted { .. }) {
                self.loaded.remove(&manifest.name);
            }
            return Err(refused);
        }
        #[cfg(test)]
        recovery::pause("update-published");
        // The stage holds nothing but its lease now. One left behind is swept
        // with the other abandoned stages.
        let _ = stage.finish(&self.recovery_root);
        // The replacement is in place, whatever then happens to the replaced
        // generation, so even a namespace change is reported with it.
        let held_at = self.plugins_dir.join(&held.entry);
        let leftover = self
            .supersede(held)
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
    /// the update that left the claims. When it is installed, every claimed
    /// generation of it is out of date, so each is marked superseded and
    /// deleted through its handle; one that cannot be is reported and left.
    /// When it is not, the one generation an interrupted update claimed is
    /// published back to `<plugins_dir>/<name>`, so the restored package is
    /// admitted only if the configured policy accepts it, like any other. An
    /// installed package of the name takes precedence over any claimed
    /// generation. Several claimed generations, or anything already at the
    /// name, are reported and left as found; a superseded generation is never
    /// put back; and a claim that holds neither is finished. Stages that
    /// interrupted updates of `name` left are swept as `remove` sweeps them.
    pub fn recover_interrupted_update(
        &mut self,
        name: &str,
    ) -> Result<UpdateRecovery, PluginError> {
        if crate::instance::validate_package_name(name).is_err() {
            return Ok(UpdateRecovery::Nothing);
        }
        #[cfg(test)]
        recovery::pause("recovery-before-lock");
        let _guard = self.recovery_root.lock()?;
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
            if !self.loaded.contains_key(name) {
                self.loaded = self.discovered()?;
            }
            return Ok(UpdateRecovery::Nothing);
        }
        self.loaded = self.discovered()?;
        let installed = self.loaded.contains_key(name);

        let mut restorable = Vec::new();
        let mut outdated = Vec::new();
        for tx in claims {
            match claim_contents(&tx)? {
                ClaimContents::Package if !installed => restorable.push(tx),
                ClaimContents::Package | ClaimContents::Superseded => outdated.push(tx),
                ClaimContents::Empty => {
                    let _ = tx.finish(&self.recovery_root);
                }
            }
        }
        let mut removed = 0;
        let mut kept = Vec::new();
        for tx in outdated {
            match self.supersede(tx)? {
                None => removed += 1,
                Some(left) => kept.push(left),
            }
        }
        let swept = if removed == 0 && kept.is_empty() {
            UpdateRecovery::Nothing
        } else {
            UpdateRecovery::Swept { removed, kept }
        };

        let held = match restorable.len() {
            0 => return Ok(swept),
            1 => restorable.remove(0),
            _ => {
                return Ok(UpdateRecovery::Ambiguous {
                    displaced: restorable
                        .iter()
                        .map(|tx| PathBuf::from(self.recovery_root.retained_path(tx)))
                        .collect(),
                });
            }
        };
        match self.recovery_root.dir.symlink_metadata(name) {
            Ok(_) => {
                return Ok(UpdateRecovery::Occupied {
                    displaced: PathBuf::from(self.recovery_root.retained_path(&held)),
                    occupant: self.plugins_dir.join(name),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        held.publish(&self.recovery_root, name)?;
        let _ = held.finish(&self.recovery_root);
        self.loaded = self.discovered()?;
        Ok(UpdateRecovery::Restored)
    }

    /// Packages an interrupted update left claimed: sorted names that are not
    /// loaded and have an abandoned claim that holds a package.
    /// [`Self::recover_interrupted_update`] puts such a package back, or, with
    /// several claims or an occupied name, reports why it cannot. Runs under
    /// the package lock, so it never holds a claim's lease while a recovery
    /// judges it.
    pub fn displaced_packages(&self) -> Result<Vec<String>, PluginError> {
        let _guard = self.recovery_root.lock()?;
        let mut current: Option<HashMap<String, LoadedPlugin>> = None;
        let mut names = Vec::new();
        for entry in self.recovery_root.dir.entries()? {
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
            if let Ok(Some(tx)) = self.recovery_root.reopen_transaction(entry_name)
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
        let mut claims = Vec::new();
        for entry in self.recovery_root.dir.entries()? {
            let entry = entry?;
            let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !recovery::is_transaction(&entry_name, name, "replacing") {
                continue;
            }
            let Some(tx) = self.recovery_root.reopen_transaction(entry_name.clone())? else {
                // An empty directory is what a process stopped while creating
                // or finishing a transaction leaves. Removing it cannot touch
                // anything else, and a held lease keeps a directory non-empty.
                self.recovery_root.check()?;
                if self.recovery_root.dir.remove_dir(&entry_name).is_ok() {
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

    /// Publish a generation `held` claimed back to `entry`, and return the
    /// error to report: `error` itself, or, when even that fails,
    /// [`PluginError::ReplacementInterrupted`]. The generation then stays in
    /// its transaction, whose lease is released on return, for recovery to
    /// put back, and the caller no longer holds the package as loaded.
    fn put_back(
        &self,
        name: &str,
        entry: &str,
        held: recovery::Transaction,
        error: PluginError,
    ) -> PluginError {
        match held.publish(&self.recovery_root, entry) {
            Ok(()) => {
                let _ = held.finish(&self.recovery_root);
                error
            }
            Err(restore) => PluginError::ReplacementInterrupted {
                name: name.to_string(),
                preserved: PathBuf::from(self.recovery_root.retained_path(&held)),
                cause: format!("{error}; moving it back: {restore}"),
            },
        }
    }

    /// Mark a claimed generation superseded, then delete it through its
    /// handle. Returns where it was left, and why, when it could not be, and
    /// refuses when the plugins directory itself changed.
    fn supersede(
        &self,
        held: recovery::Transaction,
    ) -> Result<Option<(PathBuf, String)>, PluginError> {
        let base = self.plugins_dir.join(&held.entry);
        if holds(&held.dir, recovery::PACKAGE)? {
            match held.retire(&self.recovery_root) {
                Ok(()) => {}
                Err(error @ PluginError::NamespaceChanged(_)) => return Err(error),
                Err(error) => {
                    return Ok(Some((base.join(recovery::PACKAGE), error.to_string())));
                }
            }
        }
        let deleted = held
            .dir
            .open_dir(recovery::SUPERSEDED)
            .map_err(PluginError::from)
            .and_then(|generation| {
                recovery::clear_owned(&generation)?;
                drop(generation);
                held.dir.remove_dir(recovery::SUPERSEDED)?;
                Ok(())
            });
        match deleted {
            Ok(()) => {
                let _ = held.finish(&self.recovery_root);
                Ok(None)
            }
            Err(error) => Ok(Some((base.join(recovery::SUPERSEDED), error.to_string()))),
        }
    }

    /// Delete a stage's package through its handle and release the stage. A
    /// stage that cannot be cleared is left for the sweep of abandoned stages.
    fn discard(&self, stage: recovery::Transaction) {
        let cleared = match stage.dir.open_dir(recovery::PACKAGE) {
            Ok(package) => recovery::clear_owned(&package).and_then(|()| {
                drop(package);
                stage.dir.remove_dir(recovery::PACKAGE)?;
                Ok(())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        };
        if cleared.is_ok() {
            let _ = stage.finish(&self.recovery_root);
        }
    }
}

/// Build the admitted package in `stage`, as install does.
fn stage_replacement(
    stage: &recovery::Transaction,
    manifest: &PluginManifest,
    manifest_toml: &str,
    source_dir: &Path,
    component: Option<&AdmittedComponent>,
) -> Result<(), PluginError> {
    stage.dir.create_dir(recovery::PACKAGE)?;
    let package = stage.dir.open_dir(recovery::PACKAGE)?;
    write_package(&package, manifest, manifest_toml, source_dir, component)?;
    check_staged_skills(manifest, &package)
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
    /// A generation marked superseded, which recovery only ever deletes.
    Superseded,
    /// Neither: the claim was never made or has been undone.
    Empty,
}

/// What `tx` holds. A lookup that fails for any reason but absence says
/// nothing about the claim, so it is an error, never a verdict.
fn claim_contents(tx: &recovery::Transaction) -> Result<ClaimContents, PluginError> {
    if holds(&tx.dir, recovery::PACKAGE)? {
        Ok(ClaimContents::Package)
    } else if holds(&tx.dir, recovery::SUPERSEDED)? {
        Ok(ClaimContents::Superseded)
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

fn admit_component_in(
    dir: &Dir,
    manifest: &PluginManifest,
) -> Result<Option<AdmittedComponent>, PluginError> {
    manifest
        .wasm_path
        .as_deref()
        .map(|relative| {
            validate_manifest_subpath("wasm_path", &manifest.name, relative)?;
            let mut prefix = PathBuf::new();
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
            }
            let file = dir.open(relative)?;
            if !file.metadata()?.is_file() {
                return Err(PluginError::InvalidManifest(
                    "WASM payload is not a regular file".into(),
                ));
            }
            let bytes = read_component_bytes(file.try_clone()?, file.metadata()?.len())?;
            if let Some(expected) = manifest.wasm_sha256.as_deref() {
                signature::verify_payload_digest(&bytes, expected)?;
            }
            Ok(AdmittedComponent::new(bytes))
        })
        .transpose()
}

/// Resolve a manifest executable without allowing traversal or symlink
/// indirection outside the package. This validates the pathname used for the
/// one admission read; it does not make later namespace substitutions safe.
/// A payload path that passed package confinement, carried with the canonical
/// package root and its directory identity from admission time.
pub(crate) struct ConfinedPayload {
    root: PathBuf,
    root_handle: same_file::Handle,
    path: PathBuf,
}

fn resolve_confined_wasm_path(
    plugin_dir: &Path,
    relative: &str,
) -> Result<ConfinedPayload, PluginError> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(PluginError::InvalidManifest(format!(
            "wasm_path must be a confined relative path (got {})",
            relative.display()
        )));
    }

    let root = std::fs::canonicalize(plugin_dir)?;
    let root_handle = same_file::Handle::from_path(&root)?;
    let mut candidate = root.clone();
    for component in relative.components() {
        if let Component::Normal(segment) = component {
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
        root_handle,
        path: resolved,
    })
}

/// Largest executable payload admission will read into memory.
///
/// Discovery and install read a whole payload before its digest is verified or
/// it is compiled, so without a bound an oversized file is fully retained
/// before anything has a chance to reject it. 64 MiB clears real WASM
/// components — including debug-info builds — by a wide margin while keeping
/// a single malformed or hostile package from exhausting memory.
const MAX_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;

/// Read a payload anchored to the package root that admitted it. This keeps
/// the checked admission read tied to that root; it does not claim to close
/// every filesystem namespace race.
pub(crate) fn read_stable_file(confined: &ConfinedPayload) -> Result<Vec<u8>, PluginError> {
    let ConfinedPayload {
        root,
        root_handle,
        path,
    } = confined;

    let swapped = || {
        PluginError::NamespaceChanged(format!(
            "WASM payload path changed after confinement check: {}",
            path.display()
        ))
    };

    let relative = path.strip_prefix(root).map_err(|_| {
        PluginError::InvalidManifest(format!(
            "WASM payload escaped its package root {}: {}",
            root.display(),
            path.display()
        ))
    })?;

    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload is not a regular file: {}",
            path.display()
        )));
    }

    let file = std::fs::File::open(path)?;
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

    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    if same_file::Handle::from_path(root)? != *root_handle {
        return Err(PluginError::NamespaceChanged(format!(
            "plugin package root changed after confinement check: {}",
            root.display()
        )));
    }

    let mut prefix = root.clone();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(swapped());
        };
        prefix.push(segment);
        if std::fs::symlink_metadata(&prefix)?.file_type().is_symlink() {
            return Err(swapped());
        }
    }

    if opened != same_file::Handle::from_path(path)? {
        return Err(swapped());
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
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MAX_COMPONENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_COMPONENT_BYTES {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload grew past the {MAX_COMPONENT_BYTES}-byte admission limit while being read"
        )));
    }
    Ok(bytes)
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
        | PluginError::InvalidConfig(_)
        | PluginError::InvalidInstanceId(_)
        | PluginError::InvalidEndpoint(_)
        | PluginError::LoadFailed(_)
        | PluginError::ExecutionFailed(_)
        | PluginError::PermissionDenied { .. }
        | PluginError::UnsupportedCapability(_) => false,
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
            dir.write(dest.join(entry.file_name()), std::fs::read(entry.path())?)?;
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

/// Test-only write fault injected into [`write_package`].
///
/// Production compiles this away entirely. Under test it cuts the armed write
/// off part-way, as a full disk or a killed process would, which is what lets
/// a case prove that a failure at that exact step leaves nothing behind. The
/// slot is thread-local, so a case running in parallel with others arms only
/// its own install.
#[cfg(test)]
mod write_fault {
    /// A write in [`super::write_package`] a test can make fail.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Step {
        Manifest,
        Payload,
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
            assert!(reason.contains("manifest.toml"), "{label}: {reason}");
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

        for (mode, expected) in [(0o600, "cannot be inspected"), (0o300, "cannot be listed")] {
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
            assert!(reason.contains(expected), "{mode:o}: {reason}");
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
            .recovery_root
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
            matches!(&err, PluginError::UnadmittedPackage { reason, .. } if reason.contains("admission accepts")),
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
    /// released on return. With `retired`, the claim is already marked
    /// superseded, as a finished update leaves one it could not delete.
    /// Returns the transaction's entry.
    fn abandon_claim(host: &PluginHost, name: &str, retired: bool) -> String {
        let tx = host.recovery_root.transaction(name, "replacing").unwrap();
        tx.claim(&host.recovery_root, name).unwrap();
        if retired {
            tx.retire(&host.recovery_root).unwrap();
        }
        tx.entry.clone()
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
            UpdateRecovery::Restored
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
            UpdateRecovery::Restored
        );
        assert_eq!(package_bytes(&plugins.path().join("weather")), v1);
        assert_eq!(host.get_plugin("weather").unwrap().version, "1.0.0");
        assert!(host.displaced_packages().unwrap().is_empty());
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

    /// A superseded generation is never put back, even once the package is
    /// gone: recovery only deletes it, and it is never listed as displaced.
    #[test]
    fn recovery_never_puts_back_a_superseded_generation() {
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
                displaced: expected.to_vec()
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
            UpdateRecovery::Restored
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
        let running = host
            .recovery_root
            .transaction("weather", "replacing")
            .unwrap();
        running.claim(&host.recovery_root, "weather").unwrap();

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
    /// marked superseded, and is reported there. After the package is
    /// removed, recovery still never puts it back.
    #[cfg(unix)]
    #[test]
    fn a_replaced_generation_that_cannot_be_deleted_is_kept_superseded() {
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
        assert!(left.ends_with(recovery::SUPERSEDED), "{}", left.display());
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

    /// A claimed generation recovery cannot delete is marked superseded first,
    /// so it is reported, and left, where it can never be put back.
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
        let superseded = plugins.path().join(&claim).join(recovery::SUPERSEDED);
        assert!(
            matches!(&recovered, UpdateRecovery::Swept { removed: 0, kept } if kept.len() == 1 && kept[0].0 == superseded),
            "{recovered:?}"
        );
        assert_eq!(host.get_plugin("weather").unwrap().version, "2.0.0");
        unlock(&superseded);
    }
}
