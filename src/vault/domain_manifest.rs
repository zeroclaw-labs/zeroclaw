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

/// Current v1 manifest schema version. Clients refuse v1 manifests
/// with unknown versions to surface incompatibilities loudly. v2 has
/// its own constant — see [`MANIFEST_SCHEMA_VERSION_V2`].
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// v2 manifest schema version (baseline + delta chain).
///
/// PR 1 (foundation): the data model and parser ship; the v2 client
/// behaviour (decision tree in `vault domain update`) is wired up in
/// PR 2. Until then, v1 install/update paths are unchanged.
pub const MANIFEST_SCHEMA_VERSION_V2: u32 = 2;

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

// ── Manifest v2 (baseline + cumulative delta chain) ─────────────────
//
// See `docs/domain-db-incremental-design.md` for the protocol.
// PR 1 ships only the data model + parser + validator + fetcher. The
// `vault domain update` decision tree that consumes these types lives
// in PR 2. Until then, v1 install/update paths are unchanged and
// callers that try to apply a v2 manifest get a clean error.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainManifestV2 {
    pub schema_version: u32,
    pub name: String,
    /// Top-level publication identity. Equals `baseline.version` when
    /// `deltas` is empty, otherwise equals the newest delta's version.
    pub version: String,
    pub generated_at: String,
    #[serde(default)]
    pub generator: Option<String>,
    pub baseline: BaselineSpec,
    #[serde(default)]
    pub deltas: Vec<DeltaSpec>,
    #[serde(default)]
    pub stats: ManifestStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineSpec {
    /// Immutable until the next annual cut (default: every Jan 15).
    pub version: String,
    pub url: String,
    /// Lower-case hex SHA-256 of the baseline DB bytes.
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub stats: ManifestStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaSpec {
    pub version: String,
    /// Must equal the sibling `baseline.version`. The client refuses
    /// to apply a delta whose `applies_to_baseline` doesn't match the
    /// installed baseline — that's the safety belt against mid-year
    /// schema drift or operator misconfiguration.
    pub applies_to_baseline: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub generated_at: Option<String>,
    /// Optional hint for ops dashboards / changelog. Not used for
    /// integrity decisions — the SHA gate is authoritative.
    #[serde(default)]
    pub ops: DeltaOps,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeltaOps {
    #[serde(default)]
    pub upsert: u64,
    #[serde(default)]
    pub delete: u64,
}

impl DomainManifestV2 {
    /// The newest delta in the chain, or `None` when there are no
    /// deltas yet (e.g. a freshly published annual baseline).
    pub fn latest_delta(&self) -> Option<&DeltaSpec> {
        self.deltas.last()
    }
}

/// Fetch and parse a v2 manifest from `url_or_path`. URL handling is
/// identical to [`fetch`] (the v1 entry point) — the difference is
/// the schema this returns and accepts.
pub async fn fetch_v2(url_or_path: &str) -> Result<DomainManifestV2> {
    let raw = read_manifest_text(url_or_path).await?;
    let manifest: DomainManifestV2 = serde_json::from_str(&raw)
        .with_context(|| format!("parsing v2 manifest JSON from {url_or_path}"))?;
    validate_v2(&manifest)?;
    Ok(manifest)
}

/// Validate a parsed v2 manifest. Mirrors the v1 invariants plus the
/// delta-chain shape rules.
pub fn validate_v2(m: &DomainManifestV2) -> Result<()> {
    if m.schema_version != MANIFEST_SCHEMA_VERSION_V2 {
        anyhow::bail!(
            "unsupported v2 manifest schema_version {}; this build only knows {}",
            m.schema_version,
            MANIFEST_SCHEMA_VERSION_V2
        );
    }
    if m.name.is_empty() {
        anyhow::bail!("manifest.name is empty");
    }
    if m.version.is_empty() {
        anyhow::bail!("manifest.version is empty");
    }
    validate_baseline(&m.baseline)?;

    // Delta-chain shape: every delta must apply to *this* baseline,
    // and must carry a non-empty version + valid SHA + non-zero size.
    for (idx, d) in m.deltas.iter().enumerate() {
        if d.version.is_empty() {
            anyhow::bail!("manifest.deltas[{idx}].version is empty");
        }
        if d.applies_to_baseline != m.baseline.version {
            anyhow::bail!(
                "manifest.deltas[{idx}].applies_to_baseline `{}` does not match \
                 baseline.version `{}`",
                d.applies_to_baseline,
                m.baseline.version
            );
        }
        if d.url.is_empty() {
            anyhow::bail!("manifest.deltas[{idx}].url is empty");
        }
        if !is_lower_hex_64(&d.sha256) {
            anyhow::bail!(
                "manifest.deltas[{idx}].sha256 must be a 64-char hex digest; got `{}`",
                d.sha256
            );
        }
        if d.size_bytes == 0 {
            anyhow::bail!("manifest.deltas[{idx}].size_bytes must be > 0");
        }
    }

    // Top-level `version` should equal the newest delta's version, or
    // the baseline when no deltas exist. We surface the inconsistency
    // as a hard error so the operator notices on publish.
    let expected_version = match m.deltas.last() {
        Some(last) => &last.version,
        None => &m.baseline.version,
    };
    if &m.version != expected_version {
        anyhow::bail!(
            "manifest.version `{}` does not match the chain head `{}` \
             (must equal the newest delta's version, or the baseline's \
             version when deltas is empty)",
            m.version,
            expected_version
        );
    }
    Ok(())
}

fn validate_baseline(b: &BaselineSpec) -> Result<()> {
    if b.version.is_empty() {
        anyhow::bail!("manifest.baseline.version is empty");
    }
    if b.url.is_empty() {
        anyhow::bail!("manifest.baseline.url is empty");
    }
    if !is_lower_hex_64(&b.sha256) {
        anyhow::bail!(
            "manifest.baseline.sha256 must be a 64-char hex digest; got `{}`",
            b.sha256
        );
    }
    if b.size_bytes == 0 {
        anyhow::bail!("manifest.baseline.size_bytes must be > 0");
    }
    Ok(())
}

fn is_lower_hex_64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

async fn read_manifest_text(url_or_path: &str) -> Result<String> {
    if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
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
            .with_context(|| format!("reading manifest body from {url_or_path}"))
    } else {
        std::fs::read_to_string(url_or_path)
            .with_context(|| format!("reading manifest file {url_or_path}"))
    }
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

    // ── v2 manifest tests ────────────────────────────────────────────

    fn good_baseline() -> BaselineSpec {
        BaselineSpec {
            version: "2026.01.15".into(),
            url: "https://r2.example.com/baseline.db".into(),
            sha256: "0".repeat(64),
            size_bytes: 1_000_000,
            stats: ManifestStats::default(),
        }
    }

    fn good_v2_manifest_no_deltas() -> DomainManifestV2 {
        DomainManifestV2 {
            schema_version: 2,
            name: "korean-legal".into(),
            version: "2026.01.15".into(), // == baseline.version when deltas empty
            generated_at: "2026-01-15T00:00:00Z".into(),
            generator: Some("zeroclaw test".into()),
            baseline: good_baseline(),
            deltas: vec![],
            stats: ManifestStats::default(),
        }
    }

    fn good_delta(version: &str) -> DeltaSpec {
        DeltaSpec {
            version: version.into(),
            applies_to_baseline: "2026.01.15".into(),
            url: format!("https://r2.example.com/delta-{version}.sqlite"),
            sha256: "a".repeat(64),
            size_bytes: 4_096,
            generated_at: Some(format!("{version}T00:00:00Z")),
            ops: DeltaOps {
                upsert: 5,
                delete: 0,
            },
        }
    }

    #[test]
    fn validate_v2_accepts_well_formed_no_deltas() {
        let m = good_v2_manifest_no_deltas();
        validate_v2(&m).unwrap();
    }

    #[test]
    fn validate_v2_accepts_well_formed_with_deltas() {
        let mut m = good_v2_manifest_no_deltas();
        m.deltas = vec![good_delta("2026.01.22"), good_delta("2026.04.22")];
        m.version = "2026.04.22".into(); // chain head
        validate_v2(&m).unwrap();
    }

    #[test]
    fn validate_v2_rejects_unknown_schema_version() {
        let mut m = good_v2_manifest_no_deltas();
        m.schema_version = 999;
        let err = validate_v2(&m).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn validate_v2_rejects_baseline_with_short_sha() {
        let mut m = good_v2_manifest_no_deltas();
        m.baseline.sha256 = "deadbeef".into();
        let err = validate_v2(&m).unwrap_err();
        assert!(err.to_string().contains("baseline.sha256"));
    }

    #[test]
    fn validate_v2_rejects_baseline_with_zero_size() {
        let mut m = good_v2_manifest_no_deltas();
        m.baseline.size_bytes = 0;
        let err = validate_v2(&m).unwrap_err();
        assert!(err.to_string().contains("size_bytes"));
    }

    #[test]
    fn validate_v2_rejects_delta_with_mismatched_baseline() {
        let mut m = good_v2_manifest_no_deltas();
        let mut bad = good_delta("2026.04.22");
        bad.applies_to_baseline = "2025.07.01".into();
        m.deltas = vec![bad];
        m.version = "2026.04.22".into();
        let err = validate_v2(&m).unwrap_err();
        assert!(err.to_string().contains("applies_to_baseline"));
    }

    #[test]
    fn validate_v2_rejects_top_version_not_matching_chain_head() {
        // deltas exist but `version` != newest delta's version.
        let mut m = good_v2_manifest_no_deltas();
        m.deltas = vec![good_delta("2026.01.22")];
        m.version = "2026.99.99".into(); // wrong
        let err = validate_v2(&m).unwrap_err();
        assert!(err
            .to_string()
            .contains("does not match the chain head"));
    }

    #[test]
    fn validate_v2_rejects_top_version_when_no_deltas() {
        // No deltas → top version must equal baseline.version.
        let mut m = good_v2_manifest_no_deltas();
        m.version = "drift".into();
        let err = validate_v2(&m).unwrap_err();
        assert!(err
            .to_string()
            .contains("does not match the chain head"));
    }

    #[test]
    fn latest_delta_returns_last_or_none() {
        let mut m = good_v2_manifest_no_deltas();
        assert!(m.latest_delta().is_none());
        m.deltas = vec![good_delta("2026.01.22"), good_delta("2026.04.22")];
        assert_eq!(m.latest_delta().unwrap().version, "2026.04.22");
    }

    #[tokio::test]
    async fn fetch_v2_reads_local_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        let m = good_v2_manifest_no_deltas();
        std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
        let out = fetch_v2(path.to_str().unwrap()).await.unwrap();
        assert_eq!(out.name, "korean-legal");
        assert_eq!(out.schema_version, 2);
        assert!(out.deltas.is_empty());
    }

    #[tokio::test]
    async fn fetch_v2_rejects_v1_manifest_cleanly() {
        // A v1 manifest must not be silently accepted as v2 — caller
        // is expected to dispatch on schema_version (PR 2). For now,
        // fetch_v2 must surface a clear error. We walk the anyhow
        // source chain because the top-level context is the file
        // name; the actual reason (missing `baseline`) is one level
        // down in the serde error.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        let v1 = good_manifest("http://x".into(), "0".repeat(64), 1);
        std::fs::write(&path, serde_json::to_string(&v1).unwrap()).unwrap();
        let err = fetch_v2(path.to_str().unwrap()).await.unwrap_err();
        let mut chain_text = String::new();
        for cause in err.chain() {
            chain_text.push_str(&cause.to_string());
            chain_text.push('\n');
        }
        assert!(
            chain_text.contains("baseline")
                || chain_text.contains("schema_version")
                || chain_text.contains("missing field"),
            "unexpected error chain:\n{chain_text}"
        );
    }
}
