//! Plugin host: discovery, loading, lifecycle management.

use super::error::PluginError;
use super::signature::{self, SignatureMode, VerificationResult};
use super::{PluginCapability, PluginInfo, PluginManifest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// Resolved path to the WASM file. `None` for skill-only plugins.
    wasm_path: Option<PathBuf>,
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
                    if let Err(e) = validate_manifest_shape(&manifest, &path) {
                        tracing::warn!(
                            plugin = path.display().to_string(),
                            error = %e,
                            "skipping plugin due to invalid manifest shape"
                        );
                        continue;
                    }

                    // Verify plugin signature
                    let manifest_toml = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                    match self.verify_plugin_signature(&manifest.name, &manifest_toml, &manifest) {
                        Ok(verification) => {
                            let wasm_path = manifest.wasm_path.as_deref().map(|p| path.join(p));
                            self.loaded.insert(
                                manifest.name.clone(),
                                LoadedPlugin {
                                    manifest,
                                    plugin_dir: path.clone(),
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
        self.loaded.values().map(plugin_info_from_loaded).collect()
    }

    /// Get info about a specific plugin.
    pub fn get_plugin(&self, name: &str) -> Option<PluginInfo> {
        self.loaded.get(name).map(plugin_info_from_loaded)
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

        validate_manifest_shape(&manifest, source_dir)?;

        let wasm_source = manifest.wasm_path.as_deref().map(|p| source_dir.join(p));
        if let Some(ref wasm_source) = wasm_source
            && !wasm_source.exists()
        {
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

        // Copy WASM file (if any)
        let wasm_dest = if let (Some(rel), Some(src)) = (manifest.wasm_path.as_deref(), wasm_source)
        {
            let dest = dest_dir.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dest)?;
            Some(dest)
        } else {
            None
        };

        // Copy skills/ subtree for skill-capable plugins.
        if manifest.capabilities.contains(&PluginCapability::Skill) {
            let src_skills = source_dir.join(SKILLS_SUBDIR);
            let dest_skills = dest_dir.join(SKILLS_SUBDIR);
            if src_skills.is_dir() {
                copy_dir_recursive(&src_skills, &dest_skills)?;
            }
        }

        self.loaded.insert(
            manifest.name.clone(),
            LoadedPlugin {
                manifest,
                plugin_dir: dest_dir,
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

    /// Get tool-capable plugins with their resolved WASM file paths.
    /// Returns `(manifest, resolved_wasm_path)` tuples for building `WasmTool`s.
    /// Tool plugins without a `wasm_path` are skipped.
    pub fn tool_plugin_details(&self) -> Vec<(&PluginManifest, &Path)> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Tool))
            .filter_map(|p| p.wasm_path.as_deref().map(|wp| (&p.manifest, wp)))
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

    /// Get skill-capable plugins.
    pub fn skill_plugins(&self) -> Vec<&PluginManifest> {
        self.loaded
            .values()
            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Skill))
            .map(|p| &p.manifest)
            .collect()
    }

    /// Get skill-capable plugins paired with the absolute path to their `skills/`
    /// directory. Plugins without an existing `skills/` subdirectory are skipped.
    ///
    /// Callers (typically the runtime skill loader) should pass each `skills_dir`
    /// to `load_skills_from_directory` and then re-namespace the resulting skill
    /// names as `plugin:<plugin>/<skill>` to avoid collisions with user skills.
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
    let loaded = match &p.wasm_path {
        Some(path) => path.exists(),
        // Skill-only plugins are "loaded" if their skills/ subtree exists.
        None => p.plugin_dir.join(SKILLS_SUBDIR).is_dir(),
    };
    PluginInfo {
        name: p.manifest.name.clone(),
        version: p.manifest.version.clone(),
        description: p.manifest.description.clone(),
        capabilities: p.manifest.capabilities.clone(),
        permissions: p.manifest.permissions.clone(),
        wasm_path: p.wasm_path.clone(),
        loaded,
    }
}

/// Validate manifest shape: `wasm_path` is required unless the plugin's only
/// capability is `Skill`, and `Skill` plugins must include a `skills/` directory
/// where every subdirectory holds a `SKILL.md` with the agentskills.io required
/// frontmatter fields (`name`, `description`).
fn validate_manifest_shape(
    manifest: &PluginManifest,
    plugin_dir: &Path,
) -> Result<(), PluginError> {
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
}
