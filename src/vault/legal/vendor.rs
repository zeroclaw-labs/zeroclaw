//! Offline asset cache for the legal-graph Cytoscape viewer.
//!
//! Design
//! ──────
//! The default viewer (`/legal-graph/viewer`) and the default HTML
//! snapshot use CDN-loaded Cytoscape.js. That's fine for the live
//! gateway and for online snapshot viewers, but breaks the moment a
//! snapshot is mailed to a reviewer behind a strict firewall.
//!
//! This module solves it without bloating the binary: a one-shot
//! `zeroclaw vault legal vendor-download` fetches the three required
//! JS files into `<workspace>/memory/vault_vendor/legal_graph/`, and
//! `vault legal export --offline` inlines them into the snapshot HTML
//! so the output file is fully self-contained — no network calls, no
//! CDN dependency, works on an air-gapped laptop.
//!
//! We deliberately do NOT commit the JS files to the repo: users who
//! only ever use the live gateway never need them, and committing
//! 400KB of third-party code invites stale dependencies.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Version pin — bumping these forces a re-download on next
/// `vendor-download`. Keep in lockstep with the CDN URLs in the
/// viewer template.
pub const CYTOSCAPE_VERSION: &str = "3.30.2";
pub const DAGRE_VERSION: &str = "0.8.5";
pub const CYTOSCAPE_DAGRE_VERSION: &str = "2.5.0";

/// The three files we need. Filename is what lives on disk under the
/// vendor cache directory AND what the snapshot HTML `<script>` tags
/// reference (so the output filename is stable — no hashing).
pub const ASSETS: &[(&str, &str)] = &[
    (
        "cytoscape.min.js",
        "https://unpkg.com/cytoscape@3.30.2/dist/cytoscape.min.js",
    ),
    (
        "dagre.min.js",
        "https://unpkg.com/dagre@0.8.5/dist/dagre.min.js",
    ),
    (
        "cytoscape-dagre.js",
        "https://unpkg.com/cytoscape-dagre@2.5.0/cytoscape-dagre.js",
    ),
];

pub fn vendor_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir
        .join("memory")
        .join("vault_vendor")
        .join("legal_graph")
}

pub fn asset_path(workspace_dir: &Path, filename: &str) -> PathBuf {
    vendor_dir(workspace_dir).join(filename)
}

/// Returns `true` iff all three vendor files exist with non-zero size.
/// Used by the snapshot exporter to decide whether `--offline` can
/// proceed, and by the gateway to decide whether to serve local vendor
/// files vs. let the viewer fall through to CDN.
pub fn has_all_assets(workspace_dir: &Path) -> bool {
    ASSETS.iter().all(|(name, _)| {
        asset_path(workspace_dir, name)
            .metadata()
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    })
}

/// Load a cached asset as a UTF-8 string. Returns `None` if the file
/// is missing so callers can fall back gracefully. Errors only on IO
/// failures other than `NotFound`.
pub fn load_asset(workspace_dir: &Path, filename: &str) -> Result<Option<String>> {
    let path = asset_path(workspace_dir, filename);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("reading {}: {e}", path.display())),
    }
}

/// Load a cached asset as raw bytes. Same `NotFound → None` semantics
/// as `load_asset`; use this when serving the asset over HTTP so
/// binary integrity is preserved.
pub fn load_asset_bytes(workspace_dir: &Path, filename: &str) -> Result<Option<Vec<u8>>> {
    let path = asset_path(workspace_dir, filename);
    match std::fs::read(&path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("reading {}: {e}", path.display())),
    }
}

/// Download all three assets into the workspace vendor cache, overwriting
/// any existing files if `force` is set (otherwise existing files are
/// kept to avoid hitting the CDN unnecessarily on re-runs).
pub async fn download_all(workspace_dir: &Path, force: bool) -> Result<DownloadReport> {
    let dir = vendor_dir(workspace_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating vendor dir {}", dir.display()))?;

    let client = reqwest::Client::builder()
        .user_agent(concat!("zeroclaw/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building reqwest client")?;

    let mut report = DownloadReport::default();
    for (filename, url) in ASSETS {
        let path = dir.join(filename);
        if !force {
            if let Ok(md) = path.metadata() {
                if md.len() > 0 {
                    report.skipped_existing.push(filename.to_string());
                    continue;
                }
            }
        }
        let res = client
            .get(*url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !res.status().is_success() {
            report.failed.push(format!(
                "{filename}: HTTP {} from {url}",
                res.status().as_u16()
            ));
            continue;
        }
        let bytes = res
            .bytes()
            .await
            .with_context(|| format!("reading body for {url}"))?;
        if bytes.is_empty() {
            report
                .failed
                .push(format!("{filename}: empty body from {url}"));
            continue;
        }
        std::fs::write(&path, &bytes)
            .with_context(|| format!("writing {}", path.display()))?;
        report
            .downloaded
            .push((filename.to_string(), bytes.len()));
    }
    Ok(report)
}

#[derive(Debug, Default)]
pub struct DownloadReport {
    /// `(filename, size_bytes)` for newly downloaded files.
    pub downloaded: Vec<(String, usize)>,
    pub skipped_existing: Vec<String>,
    pub failed: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_all_assets_false_on_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!has_all_assets(tmp.path()));
    }

    #[test]
    fn has_all_assets_true_when_all_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = vendor_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, _) in ASSETS {
            std::fs::write(dir.join(name), b"/* stub */").unwrap();
        }
        assert!(has_all_assets(tmp.path()));
    }

    #[test]
    fn has_all_assets_false_when_one_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = vendor_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (i, (name, _)) in ASSETS.iter().enumerate() {
            let content: &[u8] = if i == 0 { b"" } else { b"/* stub */" };
            std::fs::write(dir.join(name), content).unwrap();
        }
        assert!(!has_all_assets(tmp.path()));
    }

    #[test]
    fn load_asset_returns_none_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = load_asset(tmp.path(), "cytoscape.min.js").unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn load_asset_returns_content_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = vendor_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cytoscape.min.js"), b"console.log('hi')").unwrap();
        let r = load_asset(tmp.path(), "cytoscape.min.js").unwrap();
        assert_eq!(r.as_deref(), Some("console.log('hi')"));
    }
}
