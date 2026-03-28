//! Plugin manifest loader — scans `~/.zeroclaw/tools/` at startup.
//!
//! Layout expected on disk:
//! ```text
//! ~/.zeroclaw/tools/
//! ├── i2c_scan/
//! │   ├── tool.toml
//! │   └── i2c_scan.py
//! └── pwm_set/
//!     ├── tool.toml
//!     └── pwm_set
//! ```
//!
//! Rules:
//! - The directory is **created** if it does not exist.
//! - Each subdirectory is scanned for a `tool.toml`.
//! - Manifests that fail to parse or validate are **skipped with a warning**;
//!   they must not crash startup.
//! - Non-directory entries at the top level are silently ignored.

use super::manifest::ToolManifest;
use super::subprocess::SubprocessTool;
use crate::tools::traits::Tool;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// A successfully loaded plugin, ready for registration.
pub struct LoadedPlugin {
    /// Tool name from the manifest (unique key in [`ToolRegistry`]).
    pub name: String,
    /// Semantic version string from the manifest.
    pub version: String,
    /// The constructed tool, boxed for dynamic dispatch.
    pub tool: Box<dyn Tool>,
}

/// Scan `~/.zeroclaw/tools/` and return all valid plugins.
///
/// - Creates the directory if absent.
/// - Skips broken manifests with a `tracing::warn!` — does not propagate errors.
/// - Returns an empty `Vec` when no plugins are installed.
pub fn scan_plugin_dir() -> Vec<LoadedPlugin> {
    let tools_dir = match plugin_tools_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("[registry] cannot resolve plugin tools dir: {}", e);
            return Vec::new();
        }
    };

    // Create the directory tree if it is missing.
    if !tools_dir.exists() {
        if let Err(e) = fs::create_dir_all(&tools_dir) {
            tracing::warn!(
                "[registry] could not create {:?}: {}",
                tools_dir.display(),
                e
            );
            return Vec::new();
        }
        tracing::info!(
            "[registry] created plugin directory: {}",
            tools_dir.display()
        );
    }

    println!(
        "[registry] scanning {}...",
        match dirs_home().as_deref().filter(|s| !s.is_empty()) {
            Some(home) => tools_dir
                .to_str()
                .unwrap_or("~/.zeroclaw/tools")
                .replace(home, "~"),
            None => tools_dir
                .to_str()
                .unwrap_or("~/.zeroclaw/tools")
                .to_string(),
        }
    );

    let mut plugins = Vec::new();

    let entries = match fs::read_dir(&tools_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("[registry] cannot read tools dir: {}", e);
            return Vec::new();
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("[registry] skipping unreadable dir entry: {}", e);
                continue;
            }
        };

        let plugin_dir = entry.path();

        // Only descend into subdirectories.
        if !plugin_dir.is_dir() {
            continue;
        }

        let manifest_path = plugin_dir.join("tool.toml");

        if !manifest_path.exists() {
            tracing::debug!(
                "[registry] no tool.toml in {:?} — skipping",
                plugin_dir.file_name().unwrap_or_default()
            );
            continue;
        }

        match load_one_plugin(&plugin_dir, &manifest_path) {
            Ok(plugin) => plugins.push(plugin),
            Err(e) => {
                tracing::warn!(
                    "[registry] skipping plugin in {:?}: {}",
                    plugin_dir.file_name().unwrap_or_default(),
                    e
                );
            }
        }
    }

    plugins
}

/// Parse and validate a single plugin directory.
///
/// Returns `Err` on any validation failure so the caller can log and continue.
fn load_one_plugin(plugin_dir: &Path, manifest_path: &Path) -> Result<LoadedPlugin> {
    let raw = fs::read_to_string(manifest_path)
        .map_err(|e| anyhow::anyhow!("cannot read tool.toml: {}", e))?;

    let manifest: ToolManifest = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("TOML parse error in tool.toml: {}", e))?;

    // Validate required fields — fail fast with a descriptive error.
    if manifest.tool.name.trim().is_empty() {
        anyhow::bail!("manifest missing [tool] name");
    }
    if manifest.tool.description.trim().is_empty() {
        anyhow::bail!("manifest missing [tool] description");
    }
    if manifest.exec.binary.trim().is_empty() {
        anyhow::bail!("manifest missing [exec] binary");
    }

    // Validate binary path: must exist, be a regular file, and reside within plugin_dir.
    let canonical_plugin_dir = plugin_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot canonicalize plugin dir {}: {}",
            plugin_dir.display(),
            e
        )
    })?;
    let raw_binary_path = plugin_dir.join(&manifest.exec.binary);
    if !raw_binary_path.exists() {
        anyhow::bail!(
            "manifest exec binary not found: {}",
            raw_binary_path.display()
        );
    }
    let binary_path = raw_binary_path.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot canonicalize binary path {}: {}",
            raw_binary_path.display(),
            e
        )
    })?;
    if !binary_path.starts_with(&canonical_plugin_dir) {
        anyhow::bail!(
            "manifest exec binary escapes plugin directory: {} is not under {}",
            binary_path.display(),
            canonical_plugin_dir.display()
        );
    }
    if !binary_path.is_file() {
        anyhow::bail!(
            "manifest exec binary is not a regular file: {}",
            binary_path.display()
        );
    }

    let name = manifest.tool.name.clone();
    let version = manifest.tool.version.clone();
    let tool: Box<dyn Tool> = Box::new(SubprocessTool::new(manifest, binary_path));

    Ok(LoadedPlugin {
        name,
        version,
        tool,
    })
}

/// Return the path `~/.zeroclaw/tools/` using the `directories` crate.
pub fn plugin_tools_dir() -> Result<PathBuf> {
    use directories::BaseDirs;
    let base = BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("cannot determine the user home directory"))?;
    Ok(base.home_dir().join(".zeroclaw").join("tools"))
}

/// Best-effort home dir string for display purposes only.
fn dirs_home() -> Option<String> {
    use directories::BaseDirs;
    BaseDirs::new().map(|b| b.home_dir().to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_valid_manifest(dir: &Path) {
        let toml = r#"
[tool]
name        = "test_plugin"
version     = "1.0.0"
description = "A deterministic test plugin"

[exec]
binary = "tool.sh"

[[parameters]]
name        = "device"
type        = "string"
description = "Device alias"
required    = true
"#;
        fs::write(dir.join("tool.toml"), toml).unwrap();
        // Write a dummy binary (content doesn't matter for manifest loading).
        fs::write(
            dir.join("tool.sh"),
            "#!/bin/sh\necho '{\"success\":true,\"output\":\"ok\",\"error\":null}'\n",
        )
        .unwrap();
    }

    #[test]
    fn load_one_plugin_succeeds_for_valid_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_manifest(dir.path());

        let manifest_path = dir.path().join("tool.toml");
        let plugin = load_one_plugin(dir.path(), &manifest_path).unwrap();

        assert_eq!(plugin.name, "test_plugin");
        assert_eq!(plugin.version, "1.0.0");
        assert_eq!(plugin.tool.name(), "test_plugin");
    }

    #[test]
    fn load_one_plugin_fails_on_missing_name() {
        let dir = tempfile::tempdir().unwrap();
        let toml = r#"
[tool]
name        = ""
version     = "1.0.0"
description = "Missing name test"

[exec]
binary = "tool.sh"
"#;
        fs::write(dir.path().join("tool.toml"), toml).unwrap();

        let result = load_one_plugin(dir.path(), &dir.path().join("tool.toml"));
        match result {
            Err(e) => assert!(e.to_string().contains("name"), "unexpected error: {}", e),
            Ok(_) => panic!("expected an error for missing name"),
        }
    }

    #[test]
    fn load_one_plugin_fails_on_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("tool.toml"), "not valid toml {{{{").unwrap();

        let result = load_one_plugin(dir.path(), &dir.path().join("tool.toml"));
        match result {
            Err(e) => assert!(
                e.to_string().contains("TOML parse error"),
                "unexpected error: {}",
                e
            ),
            Ok(_) => panic!("expected a parse error"),
        }
    }

    #[test]
    fn scan_plugin_dir_skips_broken_manifests_without_panicking() {
        // We can't redirect scan_plugin_dir to an arbitrary directory (it
        // always uses ~/.zeroclaw/tools), but we can verify load_one_plugin
        // behaviour under broken input without affecting the real directory.
        let dir = tempfile::tempdir().unwrap();

        // Plugin 1: valid
        let p1 = dir.path().join("good");
        fs::create_dir_all(&p1).unwrap();
        write_valid_manifest(&p1);

        // Plugin 2: broken TOML
        let p2 = dir.path().join("bad");
        fs::create_dir_all(&p2).unwrap();
        fs::write(p2.join("tool.toml"), "{{broken").unwrap();

        // Load manually to simulate what scan_plugin_dir does.
        let good = load_one_plugin(&p1, &p1.join("tool.toml"));
        let bad = load_one_plugin(&p2, &p2.join("tool.toml"));

        assert!(good.is_ok(), "good plugin should load");
        assert!(bad.is_err(), "bad plugin should error, not panic");
    }

    #[test]
    fn plugin_tools_dir_returns_path_ending_in_zeroclaw_tools() {
        let path = plugin_tools_dir().expect("should resolve");
        let display = path.to_string_lossy();
        let expected = std::path::Path::new(".zeroclaw").join("tools");
        assert!(path.ends_with(&expected), "unexpected path: {}", display);
    }
}
