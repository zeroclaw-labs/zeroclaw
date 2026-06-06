//! Remote plugin registry: resolve a plugin name to a downloadable artifact,
//! verify it, and hand it to [`PluginHost::install`] for signature-checked
//! installation.
//!
//! This is the network half of `zeroclaw plugin install`. The local half
//! (install from a directory that already exists on disk) is handled directly
//! by `PluginHost::install`. When `install <source>` is given something that is
//! NOT an existing path, it is treated as a registry name (optionally
//! `name@version`) and resolved here.
//!
//! ## Registry format
//! The registry is a single JSON index hosted anywhere (default: a GitHub raw
//! URL). It lists plugins and a download URL for each plugin's zipped directory
//! (the same layout `PluginHost::install` expects: `manifest.toml`, the
//! `.wasm`, and an optional `skills/` subtree):
//!
//! ```json
//! {
//!   "plugins": [
//!     {
//!       "name": "ace-step",
//!       "version": "0.1.0",
//!       "description": "Self-hosted music generation",
//!       "author": "ZeroClaw Labs",
//!       "capabilities": ["tool"],
//!       "url": "https://.../ace-step-0.1.0.zip",
//!       "sha256": "<hex digest of the zip>"
//!     }
//!   ]
//! }
//! ```
//!
//! Integrity is defense-in-depth: the optional `sha256` guards the transport,
//! and the Ed25519 manifest signature (enforced by `PluginHost` per the
//! configured `signature_mode`) guards authenticity.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use zeroclaw_plugins::host::PluginHost;

/// Default public registry index (override with `--registry` or the env var).
const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw-plugins/main/registry.json";

/// Environment override for the registry URL.
const REGISTRY_URL_ENV: &str = "ZEROCLAW_PLUGIN_REGISTRY_URL";

/// Reject absurdly large downloads (plugins are small WASM modules).
const MAX_PLUGIN_ZIP_BYTES: u64 = 50 * 1024 * 1024; // 50 MiB

/// The registry index document.
#[derive(Debug, Deserialize)]
pub struct RegistryIndex {
    #[serde(default)]
    pub plugins: Vec<RegistryEntry>,
}

/// A single plugin entry in the registry.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Download URL for the zipped plugin directory.
    pub url: String,
    /// Optional hex SHA-256 of the zip (transport integrity).
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Resolve the registry URL: explicit flag > env var > built-in default.
pub fn resolve_registry_url(flag: Option<&str>) -> String {
    if let Some(url) = flag.filter(|s| !s.trim().is_empty()) {
        return url.trim().to_string();
    }
    if let Ok(url) = std::env::var(REGISTRY_URL_ENV)
        && !url.trim().is_empty()
    {
        return url.trim().to_string();
    }
    DEFAULT_REGISTRY_URL.to_string()
}

/// Split a `name` or `name@version` source string.
pub fn parse_name_version(source: &str) -> (String, Option<String>) {
    match source.split_once('@') {
        Some((name, version)) if !name.is_empty() && !version.is_empty() => {
            (name.to_string(), Some(version.to_string()))
        }
        _ => (source.to_string(), None),
    }
}

/// Find the matching entry by name (and optional exact version).
pub fn find_entry<'a>(
    index: &'a RegistryIndex,
    name: &str,
    version: Option<&str>,
) -> Result<&'a RegistryEntry> {
    match version {
        Some(want) => index
            .plugins
            .iter()
            .find(|e| e.name == name && e.version == want)
            .with_context(|| {
                if index.plugins.iter().any(|e| e.name == name) {
                    format!("plugin '{name}' has no version '{want}' in the registry")
                } else {
                    format!("plugin '{name}' not found in the registry")
                }
            }),
        // No version pin: take the last listed (registries list newest last).
        None => index
            .plugins
            .iter()
            .rfind(|e| e.name == name)
            .with_context(|| format!("plugin '{name}' not found in the registry")),
    }
}

/// Filter registry entries by a case-insensitive substring of name/description.
pub fn filter_entries<'a>(index: &'a RegistryIndex, query: &str) -> Vec<&'a RegistryEntry> {
    let q = query.to_lowercase();
    index
        .plugins
        .iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&q)
                || e.description
                    .as_deref()
                    .map(|d| d.to_lowercase().contains(&q))
                    .unwrap_or(false)
        })
        .collect()
}

/// Verify a downloaded blob against an expected hex SHA-256 (optional
/// `sha256:` prefix tolerated, case-insensitive).
pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<()> {
    let want = expected_hex
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(expected_hex.trim())
        .to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let got = hex::encode(hasher.finalize());
    if got != want {
        bail!("sha256 mismatch: expected {want}, got {got}");
    }
    Ok(())
}

/// Fetch and parse the registry index.
pub async fn fetch_index(url: &str) -> Result<RegistryIndex> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building HTTP client")?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching registry index from {url}"))?;
    if !resp.status().is_success() {
        bail!("registry returned HTTP {} for {url}", resp.status());
    }
    let text = resp.text().await.context("reading registry response")?;
    serde_json::from_str(&text).with_context(|| format!("parsing registry index from {url}"))
}

/// Download a plugin zip, enforcing the size ceiling.
async fn download_zip(entry: &RegistryEntry) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("building HTTP client")?;
    let resp = client
        .get(&entry.url)
        .send()
        .await
        .with_context(|| format!("downloading plugin from {}", entry.url))?;
    if !resp.status().is_success() {
        bail!("download returned HTTP {} for {}", resp.status(), entry.url);
    }
    if let Some(len) = resp.content_length()
        && len > MAX_PLUGIN_ZIP_BYTES
    {
        bail!("plugin archive too large: {len} bytes > {MAX_PLUGIN_ZIP_BYTES}");
    }
    let bytes = resp
        .bytes()
        .await
        .context("reading plugin archive")?
        .to_vec();
    if bytes.len() as u64 > MAX_PLUGIN_ZIP_BYTES {
        bail!(
            "plugin archive too large: {} bytes > {MAX_PLUGIN_ZIP_BYTES}",
            bytes.len()
        );
    }
    Ok(bytes)
}

/// Extract a zip into `dest`, rejecting path-traversal entries.
fn extract_zip_safe(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("opening plugin archive")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            bail!("plugin archive contains an unsafe path: {name}");
        }
        let out_path = dest.join(&name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&out_path)
                .with_context(|| format!("creating {}", out_path.display()))?;
            std::io::copy(&mut entry, &mut out)?;
        }
    }
    Ok(())
}

/// Locate the directory containing `manifest.toml` (the archive root, or a
/// single top-level subdirectory).
pub fn find_manifest_dir(root: &Path) -> Result<PathBuf> {
    if root.join("manifest.toml").exists() {
        return Ok(root.to_path_buf());
    }
    for entry in std::fs::read_dir(root).context("reading extracted archive")? {
        let p = entry?.path();
        if p.is_dir() && p.join("manifest.toml").exists() {
            return Ok(p);
        }
    }
    bail!("no manifest.toml found in the downloaded plugin archive");
}

/// Resolve `source` (a registry name or `name@version`) and install it.
pub async fn install_from_registry(
    source: &str,
    registry_flag: Option<&str>,
    host: &mut PluginHost,
) -> Result<()> {
    let url = resolve_registry_url(registry_flag);
    let (name, version) = parse_name_version(source);

    println!("Resolving '{source}' from registry {url}…");
    let index = fetch_index(&url).await?;
    let entry = find_entry(&index, &name, version.as_deref())?.clone();

    println!("Downloading {} v{} …", entry.name, entry.version);
    let bytes = download_zip(&entry).await?;
    if let Some(expected) = &entry.sha256 {
        verify_sha256(&bytes, expected)?;
        println!("  ✓ sha256 verified");
    }

    let tmp = tempfile::TempDir::new().context("creating temp dir")?;
    let extracted_root = tmp.path().join("plugin");
    extract_zip_safe(&bytes, &extracted_root)?;
    let manifest_dir = find_manifest_dir(&extracted_root)?;

    let dir_str = manifest_dir
        .to_str()
        .context("plugin path is not valid UTF-8")?;
    host.install(dir_str)
        .with_context(|| format!("installing plugin '{}'", entry.name))?;

    println!("✓ Installed {} v{}", entry.name, entry.version);
    Ok(())
}

/// Search the registry and print matching plugins.
pub async fn search_and_print(registry_flag: Option<&str>, query: &str) -> Result<()> {
    let url = resolve_registry_url(registry_flag);
    let index = fetch_index(&url).await?;
    let matches = filter_entries(&index, query);
    if matches.is_empty() {
        println!("No plugins matching '{query}' in {url}");
        return Ok(());
    }
    println!("Plugins matching '{query}' ({}):", matches.len());
    for e in matches {
        println!(
            "  {} v{} — {}",
            e.name,
            e.version,
            e.description.as_deref().unwrap_or("(no description)")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> RegistryIndex {
        serde_json::from_str(
            r#"{"plugins":[
                {"name":"ace-step","version":"0.1.0","description":"Self-hosted music","url":"https://x/a.zip"},
                {"name":"tavily","version":"0.1.0","description":"Web search","url":"https://x/t.zip","sha256":"deadbeef"},
                {"name":"tavily","version":"0.2.0","description":"Web search","url":"https://x/t2.zip"}
            ]}"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_name_and_version() {
        assert_eq!(parse_name_version("tavily"), ("tavily".into(), None));
        assert_eq!(
            parse_name_version("tavily@0.2.0"),
            ("tavily".into(), Some("0.2.0".into()))
        );
        // a lone trailing '@' is not a version pin
        assert_eq!(parse_name_version("weird@"), ("weird@".into(), None));
    }

    #[test]
    fn find_entry_unpinned_takes_newest_listed() {
        let idx = index();
        let e = find_entry(&idx, "tavily", None).unwrap();
        assert_eq!(e.version, "0.2.0");
    }

    #[test]
    fn find_entry_respects_version_pin() {
        let idx = index();
        let e = find_entry(&idx, "tavily", Some("0.1.0")).unwrap();
        assert_eq!(e.version, "0.1.0");
        assert!(e.sha256.is_some());
    }

    #[test]
    fn find_entry_errors_on_unknown_name_or_version() {
        let idx = index();
        assert!(find_entry(&idx, "nope", None).is_err());
        assert!(find_entry(&idx, "tavily", Some("9.9.9")).is_err());
    }

    #[test]
    fn search_matches_name_and_description() {
        let idx = index();
        assert_eq!(filter_entries(&idx, "music").len(), 1); // description hit
        assert_eq!(filter_entries(&idx, "tavily").len(), 2); // name hits both versions
        assert_eq!(filter_entries(&idx, "ZZZ").len(), 0);
    }

    #[test]
    fn registry_url_precedence_flag_over_default() {
        assert_eq!(
            resolve_registry_url(Some("https://custom")),
            "https://custom"
        );
        assert_eq!(resolve_registry_url(Some("  ")), DEFAULT_REGISTRY_URL);
        assert_eq!(resolve_registry_url(None), DEFAULT_REGISTRY_URL);
    }

    #[test]
    fn sha256_verifies_and_rejects() {
        // sha256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha256(b"hello", h).is_ok());
        assert!(verify_sha256(b"hello", &format!("sha256:{h}")).is_ok());
        assert!(verify_sha256(b"hello", "00ff").is_err());
    }

    #[test]
    fn find_manifest_dir_root_and_subdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // root-level manifest
        std::fs::write(tmp.path().join("manifest.toml"), "name='x'").unwrap();
        assert_eq!(find_manifest_dir(tmp.path()).unwrap(), tmp.path());

        // nested manifest in a single subdir
        let tmp2 = tempfile::TempDir::new().unwrap();
        let sub = tmp2.path().join("ace-step");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("manifest.toml"), "name='x'").unwrap();
        assert_eq!(find_manifest_dir(tmp2.path()).unwrap(), sub);

        // none
        let tmp3 = tempfile::TempDir::new().unwrap();
        assert!(find_manifest_dir(tmp3.path()).is_err());
    }
}
