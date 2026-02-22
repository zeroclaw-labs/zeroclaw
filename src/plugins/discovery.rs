//! Plugin discovery — scans directories for plugin manifests.
//!
//! Mirrors OpenClaw's `discovery.ts`: scans bundled, global, and workspace
//! extension directories for subdirectories containing `zeroclaw.plugin.toml`.

use std::path::{Path, PathBuf};

use super::manifest::{load_manifest, ManifestLoadResult, PluginManifest, PLUGIN_MANIFEST_FILENAME};
use super::registry::{DiagnosticLevel, PluginDiagnostic, PluginOrigin};

/// A discovered plugin before loading.
#[derive(Debug)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub dir: PathBuf,
    pub origin: PluginOrigin,
}

/// Result of a discovery scan.
pub struct DiscoveryResult {
    pub plugins: Vec<DiscoveredPlugin>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

/// Scan a single extensions directory for plugin subdirectories.
fn scan_dir(dir: &Path, origin: PluginOrigin) -> (Vec<DiscoveredPlugin>, Vec<PluginDiagnostic>) {
    let mut plugins = Vec::new();
    let mut diagnostics = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (plugins, diagnostics),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Skip hidden directories
        if entry
            .file_name()
            .to_str()
            .map_or(false, |n| n.starts_with('.'))
        {
            continue;
        }
        // Must contain a manifest
        if !path.join(PLUGIN_MANIFEST_FILENAME).exists() {
            continue;
        }

        match load_manifest(&path) {
            ManifestLoadResult::Ok { manifest, .. } => {
                plugins.push(DiscoveredPlugin {
                    manifest,
                    dir: path,
                    origin: origin.clone(),
                });
            }
            ManifestLoadResult::Err { error, path: mp } => {
                diagnostics.push(PluginDiagnostic {
                    level: DiagnosticLevel::Warn,
                    plugin_id: None,
                    source: Some(mp.display().to_string()),
                    message: error,
                });
            }
        }
    }

    (plugins, diagnostics)
}

/// Discover plugins from all standard locations.
///
/// Search order (later wins on ID conflict, matching OpenClaw's precedence):
/// 1. Bundled: `<binary_dir>/extensions/`
/// 2. Global: `~/.zeroclaw/extensions/`
/// 3. Workspace: `<workspace>/.zeroclaw/extensions/`
/// 4. Extra paths from config `[plugins] load_paths`
pub fn discover_plugins(
    workspace_dir: Option<&Path>,
    extra_paths: &[PathBuf],
) -> DiscoveryResult {
    let mut all_plugins = Vec::new();
    let mut all_diagnostics = Vec::new();

    // 1. Bundled — next to the binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let bundled = exe_dir.join("extensions");
            let (p, d) = scan_dir(&bundled, PluginOrigin::Bundled);
            all_plugins.extend(p);
            all_diagnostics.extend(d);
        }
    }

    // 2. Global — ~/.zeroclaw/extensions/
    if let Some(home) = dirs_home() {
        let global = home.join(".zeroclaw").join("extensions");
        let (p, d) = scan_dir(&global, PluginOrigin::Global);
        all_plugins.extend(p);
        all_diagnostics.extend(d);
    }

    // 3. Workspace — <workspace>/.zeroclaw/extensions/
    if let Some(ws) = workspace_dir {
        let ws_ext = ws.join(".zeroclaw").join("extensions");
        let (p, d) = scan_dir(&ws_ext, PluginOrigin::Workspace);
        all_plugins.extend(p);
        all_diagnostics.extend(d);
    }

    // 4. Extra paths from config
    for extra in extra_paths {
        let (p, d) = scan_dir(extra, PluginOrigin::Global);
        all_plugins.extend(p);
        all_diagnostics.extend(d);
    }

    // Deduplicate by ID — last wins (workspace overrides global overrides bundled)
    let mut seen = std::collections::HashMap::new();
    for (i, plugin) in all_plugins.iter().enumerate() {
        seen.insert(plugin.manifest.id.clone(), i);
    }
    let mut deduped: Vec<DiscoveredPlugin> = Vec::with_capacity(seen.len());
    // Collect in insertion order of the winning index
    let mut indices: Vec<usize> = seen.values().copied().collect();
    indices.sort();
    for i in indices {
        deduped.push(all_plugins.swap_remove(i));
    }

    DiscoveryResult {
        plugins: deduped,
        diagnostics: all_diagnostics,
    }
}

fn dirs_home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_plugin_dir(parent: &Path, id: &str) {
        let dir = parent.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(PLUGIN_MANIFEST_FILENAME),
            format!(
                r#"
id = "{id}"
name = "Test {id}"
version = "0.1.0"
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn discover_from_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("project");
        let ext_dir = ws.join(".zeroclaw").join("extensions");
        fs::create_dir_all(&ext_dir).unwrap();
        make_plugin_dir(&ext_dir, "my-plugin");

        let result = discover_plugins(Some(&ws), &[]);
        assert!(result.plugins.iter().any(|p| p.manifest.id == "my-plugin"));
    }

    #[test]
    fn discover_from_extra_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let ext_dir = tmp.path().join("custom-plugins");
        fs::create_dir_all(&ext_dir).unwrap();
        make_plugin_dir(&ext_dir, "custom-one");

        let result = discover_plugins(None, &[ext_dir]);
        assert!(result
            .plugins
            .iter()
            .any(|p| p.manifest.id == "custom-one"));
    }

    #[test]
    fn discover_skips_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let ext_dir = tmp.path().join("ext");
        fs::create_dir_all(&ext_dir).unwrap();
        make_plugin_dir(&ext_dir, ".hidden-plugin");
        make_plugin_dir(&ext_dir, "visible-plugin");

        let (plugins, _) = super::scan_dir(&ext_dir, PluginOrigin::Workspace);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.id, "visible-plugin");
    }

    #[test]
    fn discover_records_bad_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let ext_dir = tmp.path().join("ext");
        let bad = ext_dir.join("bad-plugin");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join(PLUGIN_MANIFEST_FILENAME), "not valid toml {{{{").unwrap();

        let (plugins, diagnostics) = super::scan_dir(&ext_dir, PluginOrigin::Workspace);
        assert!(plugins.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].level, DiagnosticLevel::Warn);
    }
}
