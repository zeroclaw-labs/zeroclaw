//! Plugin host: discovery, loading, lifecycle management.

use super::error::PluginError;
use super::signature::{self, SignatureMode};
use super::{PluginCapability, PluginInfo, PluginManifest};
use crate::config::validate_manifest_config;
use std::collections::HashMap;
use std::io::Read;
use std::path::Component;
use std::path::{Path, PathBuf};
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
    plugin_dir: PathBuf,
    component: Option<AdmittedComponent>,
}

/// Exact executable bytes that passed package confinement and digest policy.
///
/// Adapters consume this artifact instead of reopening a manifest path, so the
/// component they compile is the same file generation the host admitted.
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

    #[cfg(all(test, feature = "plugins-wasmtime"))]
    pub(crate) fn test_component(bytes: impl Into<Vec<u8>>) -> Self {
        Self::new(bytes.into())
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

        let entries = std::fs::read_dir(&self.plugins_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                let manifest_path = path.join("manifest.toml");
                if manifest_path.exists()
                    && let Ok((manifest, manifest_toml)) = self.load_manifest(&manifest_path)
                {
                    if path.file_name().and_then(|name| name.to_str())
                        != Some(manifest.name.as_str())
                    {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "manifest_name": manifest.name.clone()})), "skipping plugin whose manifest name does not match its directory");
                        continue;
                    }
                    if let Err(e) = validate_manifest_shape(&manifest, &path) {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to invalid manifest shape");
                        continue;
                    }

                    if let Err(e) =
                        self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest)
                    {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to signature verification failure");
                        continue;
                    }
                    if let Err(e) = validate_manifest_config(&manifest) {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": path.display().to_string(), "error": format!("{}", e)})), "skipping plugin due to invalid config schema");
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
                            plugin_dir: path,
                            component,
                        },
                    );
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
    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        self.loaded.values().map(plugin_info_from_loaded).collect()
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

    /// Install a plugin from a directory path. Returns the installed
    /// plugin's manifest name so callers can key follow-up work (config
    /// seeding, messaging) off the canonical name rather than the source path.
    pub fn install(&mut self, source: &str) -> Result<String, PluginError> {
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
            .ok_or_else(|| PluginError::InvalidManifest("no parent directory".into()))?;

        validate_manifest_shape(&manifest, source_dir)?;

        if self.loaded.contains_key(&manifest.name) {
            return Err(PluginError::AlreadyLoaded(manifest.name));
        }

        // Parse, verify, and persist the same manifest generation.
        self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest)?;
        validate_manifest_config(&manifest)?;
        let component = admit_component(source_dir, &manifest)?;

        let dest_dir = self.plugins_dir.join(&manifest.name);
        if dest_dir.exists() {
            return Err(PluginError::AlreadyLoaded(manifest.name));
        }
        std::fs::create_dir_all(&dest_dir)?;

        // Persist the exact manifest and payload generations admitted above.
        std::fs::write(dest_dir.join("manifest.toml"), manifest_toml.as_bytes())?;
        if let (Some(rel), Some(component)) = (manifest.wasm_path.as_deref(), component.as_ref()) {
            let dest = dest_dir.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, component.bytes())?;
            resolve_confined_wasm_path(&dest_dir, rel)?;
        }

        // Copy skills/ subtree for skill-capable plugins.
        if manifest.capabilities.contains(&PluginCapability::Skill) {
            let src_skills = source_dir.join(SKILLS_SUBDIR);
            let dest_skills = dest_dir.join(SKILLS_SUBDIR);
            if src_skills.is_dir() {
                copy_dir_recursive(&src_skills, &dest_skills)?;
            }
        }

        let installed_name = manifest.name.clone();
        self.loaded.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
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

    /// Get tool-capable plugins with the exact executable bytes admitted for
    /// them. Plugins without an executable artifact are skipped.
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
        self.executable_plugin_details(PluginCapability::Channel)
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
    let wasm_path = p
        .manifest
        .wasm_path
        .as_deref()
        .map(|relative| p.plugin_dir.join(relative));
    PluginInfo {
        name: p.manifest.name.clone(),
        version: p.manifest.version.clone(),
        description: p.manifest.description.clone(),
        capabilities: p.manifest.capabilities.clone(),
        permissions: p.manifest.permissions.clone(),
        wasm_path,
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
            let path = resolve_confined_wasm_path(plugin_dir, relative)?;
            let bytes = read_stable_file(&path)?;
            if let Some(expected) = manifest.wasm_sha256.as_deref() {
                signature::verify_payload_digest(&bytes, expected)?;
            }
            Ok(AdmittedComponent::new(bytes))
        })
        .transpose()
}

/// Resolve a manifest's executable path without allowing traversal or symlink
/// indirection outside the package. Every existing component is checked before
/// canonicalization so an in-package symlink is rejected as well.
fn resolve_confined_wasm_path(plugin_dir: &Path, relative: &str) -> Result<PathBuf, PluginError> {
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
    Ok(resolved)
}

/// Open the canonical payload once and confirm that the opened file object is
/// still the object named by that path before returning its bytes.
pub(crate) fn read_stable_file(path: &Path) -> Result<Vec<u8>, PluginError> {
    let file_name = path.file_name().ok_or_else(|| {
        PluginError::InvalidManifest(format!(
            "WASM payload path has no file name: {}",
            path.display()
        ))
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let expected = std::fs::canonicalize(parent)?.join(file_name);
    let mut file = std::fs::File::open(&expected)?;
    if !file.metadata()?.is_file() {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload is not a regular file: {}",
            expected.display()
        )));
    }

    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    let resolved = std::fs::canonicalize(&expected)?;
    let current = same_file::Handle::from_path(&resolved)?;
    if resolved != expected || opened != current {
        return Err(PluginError::InvalidManifest(format!(
            "WASM payload path changed after confinement check: {}",
            expected.display()
        )));
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Validate manifest shape: `wasm_path` is required unless the plugin's only
/// capability is `Skill`, and `Skill` plugins must include a `skills/` directory
/// where every subdirectory holds a `SKILL.md` with the agentskills.io required
/// frontmatter fields (`name`, `description`).
fn validate_manifest_shape(
    manifest: &PluginManifest,
    plugin_dir: &Path,
) -> Result<(), PluginError> {
    crate::instance::validate_package_name(&manifest.name).map_err(PluginError::InvalidManifest)?;

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
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();

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
        std::fs::write(stray.join("p.wasm"), b"\0asm").unwrap();

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
    fn discovery_rejects_package_directory_name_mismatches() {
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
            std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
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

    fn signed_tool_manifest(name: &str, payload: &[u8], include_digest: bool) -> (String, String) {
        let digest = if include_digest {
            format!("wasm_sha256 = \"{}\"\n", signature::sha256_hex(payload))
        } else {
            String::new()
        };
        let unsigned = format!(
            "name = \"{name}\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\n{digest}capabilities = [\"tool\"]\n"
        );
        let (private_key, publisher_key) = signature::generate_signing_key().unwrap();
        let signed = format!(
            "{unsigned}publisher_key = \"{publisher_key}\"\nsignature = \"{}\"\n",
            signature::sign_manifest(&unsigned, &private_key).unwrap()
        );
        (signed, publisher_key)
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
    fn install_rejects_absolute_and_parent_wasm_paths() {
        let source = tempdir().unwrap();
        let outside_root = tempdir().unwrap();
        let outside = outside_root.path().join("outside-plugin.wasm");
        std::fs::write(&outside, b"outside").unwrap();
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir(plugins.path()).unwrap();

        for wasm_path in [
            "../outside-plugin.wasm".to_string(),
            outside.display().to_string(),
        ] {
            std::fs::write(
                source.path().join("manifest.toml"),
                format!(
                    "name = \"unsafe-path\"\nversion = \"0.1.0\"\nwasm_path = \"{wasm_path}\"\ncapabilities = [\"tool\"]\n"
                ),
            )
            .unwrap();
            assert!(matches!(
                host.install(source.path().to_str().unwrap()),
                Err(PluginError::InvalidManifest(_))
            ));
            assert!(!plugins.path().join("unsafe-path").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_payload_and_package_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let plugins = root.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let outside = root.path().join("outside.wasm");
        std::fs::write(&outside, b"outside").unwrap();

        let payload_link = plugins.join("payload-link");
        std::fs::create_dir_all(&payload_link).unwrap();
        std::fs::write(
            payload_link.join("manifest.toml"),
            "name = \"payload-link\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        symlink(&outside, payload_link.join("plugin.wasm")).unwrap();

        let package_target = root.path().join("package-target");
        std::fs::create_dir_all(&package_target).unwrap();
        std::fs::write(
            package_target.join("manifest.toml"),
            "name = \"package-link\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        std::fs::write(package_target.join("plugin.wasm"), b"component").unwrap();
        symlink(&package_target, plugins.join("package-link")).unwrap();

        let host = PluginHost::new(root.path()).unwrap();
        assert!(host.list_plugins().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn stable_payload_read_rejects_a_post_confinement_symlink_swap() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let payload = root.path().join("plugin.wasm");
        std::fs::write(&payload, b"inside").unwrap();
        let confined = std::fs::canonicalize(&payload).unwrap();
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
    fn declared_payload_digest_is_enforced_in_non_strict_modes() {
        let payload = b"actual component";
        for mode in [SignatureMode::Disabled, SignatureMode::Permissive] {
            let root = tempdir().unwrap();
            let plugin_dir = root.path().join("digest-mismatch");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(plugin_dir.join("plugin.wasm"), payload).unwrap();
            std::fs::write(
                plugin_dir.join("manifest.toml"),
                format!(
                    "name = \"digest-mismatch\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\nwasm_sha256 = \"{}\"\ncapabilities = [\"tool\"]\n",
                    signature::sha256_hex(b"different component")
                ),
            )
            .unwrap();

            let host =
                PluginHost::from_plugins_dir_with_security(root.path(), mode, Vec::new()).unwrap();
            assert!(host.list_plugins().is_empty());
        }
    }

    #[test]
    fn strict_mode_requires_and_accepts_a_signed_payload_digest() {
        let payload = b"signed component";
        for (name, include_digest, expected_count) in
            [("no-digest", false, 0), ("with-digest", true, 1)]
        {
            let root = tempdir().unwrap();
            let plugin_dir = root.path().join(name);
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(plugin_dir.join("plugin.wasm"), payload).unwrap();
            let (manifest, publisher_key) = signed_tool_manifest(name, payload, include_digest);
            std::fs::write(plugin_dir.join("manifest.toml"), manifest).unwrap();

            let host = PluginHost::from_plugins_dir_with_security(
                root.path(),
                SignatureMode::Strict,
                vec![publisher_key],
            )
            .unwrap();
            assert_eq!(host.list_plugins().len(), expected_count);
        }
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

    #[test]
    fn strict_install_persists_the_exact_signed_generations() {
        let payload = b"signed install component";
        let source = tempdir().unwrap();
        std::fs::write(source.path().join("plugin.wasm"), payload).unwrap();
        let (manifest, publisher_key) = signed_tool_manifest("signed-install", payload, true);
        std::fs::write(source.path().join("manifest.toml"), &manifest).unwrap();
        let plugins = tempdir().unwrap();
        let mut host = PluginHost::from_plugins_dir_with_security(
            plugins.path(),
            SignatureMode::Strict,
            vec![publisher_key],
        )
        .unwrap();

        host.install(source.path().to_str().unwrap()).unwrap();
        let installed = plugins.path().join("signed-install");
        assert_eq!(
            std::fs::read_to_string(installed.join("manifest.toml")).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read(installed.join("plugin.wasm")).unwrap(),
            payload
        );
        assert_eq!(host.tool_plugin_details()[0].1.bytes(), payload);
    }

    #[test]
    fn digest_without_an_executable_path_is_invalid() {
        let manifest: PluginManifest = toml::from_str(
            "name = \"digest-only\"\nversion = \"0.1.0\"\nwasm_sha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\ncapabilities = [\"skill\"]\n",
        )
        .unwrap();
        let root = tempdir().unwrap();
        assert!(matches!(
            validate_manifest_shape(&manifest, root.path()),
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
