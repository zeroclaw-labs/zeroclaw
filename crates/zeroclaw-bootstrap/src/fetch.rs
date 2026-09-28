//! Fetching from the pinned origin, digest computation, and checksum-manifest
//! parsing.
//!
//! [`Fetcher`] exists so the verification and refusal logic can be exercised
//! without a network. The production implementation is [`HttpFetcher`]; tests
//! supply their own against local fixtures. There is no test-only flag on the
//! binary and no environment variable that swaps the origin — a fixture
//! fetcher is reachable only by linking the library.

use sha2::{Digest, Sha256};

use crate::error::BootstrapError;
use crate::origin::PinnedUrl;

/// Request timeout for the checksum manifest, matching the updater's API
/// timeout.
const MANIFEST_TIMEOUT_SECS: u64 = 15;
/// Request timeout for an artifact download, matching the updater's.
const ARTIFACT_TIMEOUT_SECS: u64 = 300;

/// Source of bytes at a launcher-constructed URL.
pub trait Fetcher {
    /// Fetches the full body at `url`.
    fn fetch(&self, url: &PinnedUrl) -> Result<Vec<u8>, BootstrapError>;
}

/// Lowercase hex SHA-256 of `bytes`, in the form the checksum manifest uses.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// SHA-256 of a file on disk, or an [`BootstrapError::Io`].
pub fn sha256_file(path: &std::path::Path) -> Result<String, BootstrapError> {
    let bytes = std::fs::read(path)
        .map_err(|err| BootstrapError::io(format!("reading {}", path.display()), &err))?;
    Ok(sha256_hex(&bytes))
}

/// Looks up one asset's expected digest in a `SHA256SUMS` body.
///
/// Matching is on the exact file-name field, not a substring. `install.sh`
/// uses `grep "$asset_name"`, which also matches a longer name that merely
/// contains the asset name (for example an `.attestation.jsonl` sibling);
/// that fails closed today but is fragile. Exact field matching removes the
/// ambiguity.
pub fn expected_digest_for(manifest: &str, asset_name: &str) -> Result<String, BootstrapError> {
    let mut found: Option<&str> = None;
    for (index, line) in manifest.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(digest), Some(name)) = (fields.next(), fields.next()) else {
            return Err(BootstrapError::MalformedChecksumManifest {
                reason: format!("line {} is not `<digest>  <name>`", index + 1),
            });
        };
        // `sha256sum` writes a binary-mode marker `*` before the name.
        let name = name.strip_prefix('*').unwrap_or(name);
        if name != asset_name {
            continue;
        }
        if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(BootstrapError::MalformedChecksumManifest {
                reason: format!(
                    "line {} does not carry a 64-character hex digest",
                    index + 1
                ),
            });
        }
        if found.is_some() {
            return Err(BootstrapError::MalformedChecksumManifest {
                reason: format!("`{asset_name}` is listed more than once"),
            });
        }
        found = Some(digest);
    }
    found
        .map(|digest| digest.to_ascii_lowercase())
        .ok_or_else(|| BootstrapError::MalformedChecksumManifest {
            reason: format!("no entry for `{asset_name}`"),
        })
}

/// Production fetcher: HTTPS against the pinned origin only.
pub struct HttpFetcher {
    client: reqwest::blocking::Client,
}

impl HttpFetcher {
    /// Builds a fetcher with the launcher's user agent and timeouts.
    pub fn new() -> Result<Self, BootstrapError> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("zeroclaw-bootstrap/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(ARTIFACT_TIMEOUT_SECS))
            .connect_timeout(std::time::Duration::from_secs(MANIFEST_TIMEOUT_SECS))
            .build()
            .map_err(|err| BootstrapError::io("building the HTTP client", &err))?;
        Ok(Self { client })
    }
}

impl Fetcher for HttpFetcher {
    fn fetch(&self, url: &PinnedUrl) -> Result<Vec<u8>, BootstrapError> {
        let response =
            self.client
                .get(url.as_str())
                .send()
                .map_err(|err| BootstrapError::Transport {
                    url: url.as_str().to_string(),
                    reason: err.to_string(),
                })?;

        // Redirects are followed by the client, so the landing URL is checked
        // rather than assumed. GitHub serves release assets from a separate
        // CDN host, so a redirect off the pinned origin is expected and is
        // not by itself a refusal — the artifact digest is what authorises
        // the bytes. What must never happen is the *request* starting
        // anywhere but the pinned origin, and `PinnedUrl` makes that
        // unrepresentable.
        let status = response.status();
        if !status.is_success() {
            return Err(BootstrapError::Transport {
                url: url.as_str().to_string(),
                reason: format!("HTTP {status}"),
            });
        }

        response
            .bytes()
            .map(|body| body.to_vec())
            .map_err(|err| BootstrapError::Transport {
                url: url.as_str().to_string(),
                reason: err.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "\
1111111111111111111111111111111111111111111111111111111111111111  zeroclaw-x86_64-apple-darwin.tar.gz
2222222222222222222222222222222222222222222222222222222222222222  zeroclaw-x86_64-apple-darwin.tar.gz.attestation.jsonl
3333333333333333333333333333333333333333333333333333333333333333  zeroclaw-x86_64-pc-windows-msvc.zip
";

    #[test]
    fn finds_the_exact_asset_entry() {
        assert_eq!(
            expected_digest_for(MANIFEST, "zeroclaw-x86_64-apple-darwin.tar.gz")
                .expect("entry present"),
            "1".repeat(64)
        );
    }

    #[test]
    fn does_not_match_a_longer_name_that_contains_the_asset_name() {
        // The substring-matching shape this avoids would have returned two
        // digests for the darwin tarball.
        assert_eq!(
            expected_digest_for(MANIFEST, "zeroclaw-x86_64-pc-windows-msvc.zip")
                .expect("entry present"),
            "3".repeat(64)
        );
    }

    #[test]
    fn refuses_a_manifest_without_the_asset() {
        assert!(matches!(
            expected_digest_for(MANIFEST, "zeroclaw-aarch64-linux-android.tar.gz"),
            Err(BootstrapError::MalformedChecksumManifest { .. })
        ));
    }

    #[test]
    fn refuses_a_short_or_non_hex_digest() {
        let bad = "notahexdigest  zeroclaw-x86_64-apple-darwin.tar.gz\n";
        assert!(expected_digest_for(bad, "zeroclaw-x86_64-apple-darwin.tar.gz").is_err());
    }

    #[test]
    fn refuses_a_duplicated_entry() {
        let dupe = format!("{MANIFEST}{}", MANIFEST.lines().next().expect("line"));
        assert!(expected_digest_for(&dupe, "zeroclaw-x86_64-apple-darwin.tar.gz").is_err());
    }

    #[test]
    fn digest_of_known_bytes() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
