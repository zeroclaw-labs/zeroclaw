//! Step B2 — Domain corpus manifest format + downloader.
//!
//! A **manifest** is a small JSON document describing where to fetch a
//! domain.db bundle and how to verify its integrity. We keep the wire
//! format intentionally minimal so it's hand-editable and survives
//! tooling changes:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "name": "korean-legal",
//!   "version": "2026.01",
//!   "generated_at": "2026-01-15T00:00:00Z",
//!   "generator": "zeroclaw 0.1.7",
//!   "bundle": {
//!     "url": "https://r2.example.com/moa/domain/korean-legal-2026.01.db",
//!     "sha256": "abc123…",
//!     "size_bytes": 1234567890,
//!     "compression": "none"
//!   },
//!   "stats": { "vault_documents": 50000, "vault_links": 200000 }
//! }
//! ```
//!
//! Distribution model
//! ──────────────────
//! Operators publish:
//!   1. The bundle file (`*.db`) to a public/private bucket (Cloudflare R2,
//!      S3, anything HTTP-reachable).
//!   2. A manifest file pointing at it.
//!
//! Clients (`vault domain install --from <manifest_url>`) fetch the
//! manifest first, verify the published `sha256`, then stream the
//! bundle to a staging path and **atomic rename** into
//! `<workspace>/memory/domain.db` via `domain::install_from`.
//!
//! Why a separate manifest?
//! ────────────────────────
//! - Allows the manifest URL to be pinned in config while the bundle
//!   URL rotates (e.g. CDN edge cache invalidation).
//! - Lets us include checksums + compression hints without baking that
//!   info into the binary.
//! - Future-proof for differential updates: the manifest can grow a
//!   `delta` field pointing at a sparse update file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Current manifest schema version. Clients refuse manifests with
/// unknown versions to surface incompatibilities loudly.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainManifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    pub generated_at: String,
    #[serde(default)]
    pub generator: Option<String>,
    pub bundle: BundleSpec,
    #[serde(default)]
    pub stats: ManifestStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleSpec {
    pub url: String,
    /// Lower-case hex SHA-256 of the bundle bytes.
    pub sha256: String,
    pub size_bytes: u64,
    /// `"none"` | `"zstd"` (zstd reserved for future use; current client
    /// only accepts `"none"` so the implementation stays trivial).
    #[serde(default = "default_compression")]
    pub compression: String,
}

fn default_compression() -> String {
    "none".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestStats {
    #[serde(default)]
    pub vault_documents: u64,
    #[serde(default)]
    pub vault_links: u64,
    #[serde(default)]
    pub laws: u64,
    #[serde(default)]
    pub cases: u64,
}

/// Fetch a manifest from `url_or_path`. If the input starts with
/// `http://` or `https://` it's treated as a URL; otherwise as a local
/// filesystem path. Local paths are useful for air-gapped installs and
/// for the test suite.
pub async fn fetch(url_or_path: &str) -> Result<DomainManifest> {
    let raw = if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
        let client = http_client()?;
        let res = client
            .get(url_or_path)
            .send()
            .await
            .with_context(|| format!("GET {url_or_path}"))?;
        if !res.status().is_success() {
            anyhow::bail!(
                "manifest fetch failed: HTTP {} from {url_or_path}",
                res.status().as_u16()
            );
        }
        res.text()
            .await
            .with_context(|| format!("reading manifest body from {url_or_path}"))?
    } else {
        std::fs::read_to_string(url_or_path)
            .with_context(|| format!("reading manifest file {url_or_path}"))?
    };
    let manifest: DomainManifest = serde_json::from_str(&raw)
        .with_context(|| format!("parsing manifest JSON from {url_or_path}"))?;
    validate(&manifest)?;
    Ok(manifest)
}

/// Reject malformed manifests at parse time so downstream code can
/// trust the struct. Validates schema version, compression scheme,
/// and basic shape sanity.
pub fn validate(m: &DomainManifest) -> Result<()> {
    if m.schema_version != MANIFEST_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported manifest schema_version {}; this build only knows {}",
            m.schema_version,
            MANIFEST_SCHEMA_VERSION
        );
    }
    if m.name.is_empty() {
        anyhow::bail!("manifest.name is empty");
    }
    if m.version.is_empty() {
        anyhow::bail!("manifest.version is empty");
    }
    if m.bundle.url.is_empty() {
        anyhow::bail!("manifest.bundle.url is empty");
    }
    if m.bundle.sha256.len() != 64 || !m.bundle.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!(
            "manifest.bundle.sha256 must be a 64-char hex digest; got `{}`",
            m.bundle.sha256
        );
    }
    if m.bundle.size_bytes == 0 {
        anyhow::bail!("manifest.bundle.size_bytes must be > 0");
    }
    if m.bundle.compression != "none" {
        anyhow::bail!(
            "unsupported manifest.bundle.compression `{}` (only `none` is implemented)",
            m.bundle.compression
        );
    }
    Ok(())
}

/// Download the bundle pointed to by `manifest` into `dest_dir` and
/// verify its SHA-256 against `manifest.bundle.sha256`. Returns the
/// staging path on success. The caller is responsible for the atomic
/// rename into `<workspace>/memory/domain.db` via
/// [`crate::vault::domain::install_from`].
///
/// On checksum mismatch the staging file is removed and an error is
/// returned, so a corrupted download cannot be installed by accident.
pub async fn download_bundle(manifest: &DomainManifest, dest_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("creating staging dir {}", dest_dir.display()))?;
    let staging = dest_dir.join(format!(
        "{}-{}.db.staging",
        sanitize(&manifest.name),
        sanitize(&manifest.version)
    ));

    let url = &manifest.bundle.url;
    let raw = if url.starts_with("http://") || url.starts_with("https://") {
        let client = http_client()?;
        let res = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !res.status().is_success() {
            anyhow::bail!(
                "bundle fetch failed: HTTP {} from {url}",
                res.status().as_u16()
            );
        }
        res.bytes()
            .await
            .with_context(|| format!("reading bundle body from {url}"))?
            .to_vec()
    } else {
        // Local path — primarily for tests and air-gapped installs.
        std::fs::read(url).with_context(|| format!("reading bundle file {url}"))?
    };

    if (raw.len() as u64) != manifest.bundle.size_bytes {
        anyhow::bail!(
            "bundle size mismatch: manifest declared {} bytes, downloaded {}",
            manifest.bundle.size_bytes,
            raw.len()
        );
    }
    let digest = hex::encode(Sha256::digest(&raw));
    if !digest.eq_ignore_ascii_case(&manifest.bundle.sha256) {
        anyhow::bail!(
            "bundle SHA-256 mismatch: manifest declared {}, downloaded {}",
            manifest.bundle.sha256,
            digest
        );
    }
    std::fs::write(&staging, &raw)
        .with_context(|| format!("writing staging file {}", staging.display()))?;
    Ok(staging)
}

/// Compute SHA-256 of a file streamingly (kept here so `vault domain
/// build` can produce manifest entries without re-implementing the
/// digest). Returns lower-case hex.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("zeroclaw/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()
        .context("building reqwest client for manifest fetch")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_manifest(bundle_url: String, sha: String, size: u64) -> DomainManifest {
        DomainManifest {
            schema_version: 1,
            name: "korean-legal".into(),
            version: "2026.01".into(),
            generated_at: "2026-01-15T00:00:00Z".into(),
            generator: Some("zeroclaw test".into()),
            bundle: BundleSpec {
                url: bundle_url,
                sha256: sha,
                size_bytes: size,
                compression: "none".into(),
            },
            stats: ManifestStats::default(),
        }
    }

    #[test]
    fn validate_rejects_unknown_schema_version() {
        let mut m = good_manifest("http://x".into(), "0".repeat(64), 1);
        m.schema_version = 999;
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn validate_rejects_short_or_nonhex_sha() {
        let mut m = good_manifest("http://x".into(), "deadbeef".into(), 1);
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("sha256"));
        m.bundle.sha256 = "Z".repeat(64);
        assert!(validate(&m).is_err());
    }

    #[test]
    fn validate_rejects_unsupported_compression() {
        let mut m = good_manifest("http://x".into(), "0".repeat(64), 1);
        m.bundle.compression = "zstd".into();
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("compression"));
    }

    #[test]
    fn validate_accepts_well_formed() {
        let m = good_manifest("http://x".into(), "0".repeat(64), 1);
        validate(&m).unwrap();
    }

    #[tokio::test]
    async fn fetch_reads_local_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        let m = good_manifest("file:///x".into(), "0".repeat(64), 1);
        std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
        let out = fetch(path.to_str().unwrap()).await.unwrap();
        assert_eq!(out.name, "korean-legal");
    }

    #[tokio::test]
    async fn download_bundle_verifies_sha256() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bundle_path = tmp.path().join("source.db");
        let bundle_bytes = b"fake sqlite db bytes for test".to_vec();
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();
        let sha = hex::encode(Sha256::digest(&bundle_bytes));

        let m = good_manifest(
            bundle_path.to_string_lossy().into_owned(),
            sha,
            bundle_bytes.len() as u64,
        );

        let staging = download_bundle(&m, tmp.path()).await.unwrap();
        assert!(staging.exists());
        assert_eq!(std::fs::read(&staging).unwrap(), bundle_bytes);
    }

    #[tokio::test]
    async fn download_bundle_rejects_sha_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bundle_path = tmp.path().join("source.db");
        let bundle_bytes = b"actual content".to_vec();
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();
        let wrong_sha = "a".repeat(64);

        let m = good_manifest(
            bundle_path.to_string_lossy().into_owned(),
            wrong_sha,
            bundle_bytes.len() as u64,
        );
        let err = download_bundle(&m, tmp.path()).await.unwrap_err();
        assert!(err.to_string().contains("SHA-256 mismatch"));
    }

    #[tokio::test]
    async fn download_bundle_rejects_size_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bundle_path = tmp.path().join("source.db");
        let bundle_bytes = b"actual content".to_vec();
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();
        let sha = hex::encode(Sha256::digest(&bundle_bytes));

        let mut m = good_manifest(
            bundle_path.to_string_lossy().into_owned(),
            sha,
            (bundle_bytes.len() + 100) as u64,
        );
        m.bundle.size_bytes = (bundle_bytes.len() + 100) as u64;
        let err = download_bundle(&m, tmp.path()).await.unwrap_err();
        assert!(err.to_string().contains("size mismatch"));
    }

    #[test]
    fn sha256_file_matches_hex_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("blob");
        let bytes = b"hello world".to_vec();
        std::fs::write(&path, &bytes).unwrap();
        let (digest, size) = sha256_file(&path).unwrap();
        let expected = hex::encode(Sha256::digest(&bytes));
        assert_eq!(digest, expected);
        assert_eq!(size, bytes.len() as u64);
    }
}
