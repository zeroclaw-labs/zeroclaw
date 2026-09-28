//! The pinned release origin and the only URLs the launcher will ever build.
//!
//! The launcher exposes no URL argument. Every request it makes is a
//! [`PinnedUrl`], and a `PinnedUrl` can only be produced by joining the
//! compile-time origin constant with a validated [`ReleaseTag`] and an asset
//! name generated from the canonical registry. There is no constructor that
//! accepts a caller-supplied string, so "download from somewhere else" is not
//! a state this program can represent.

use crate::error::BootstrapError;

/// Origin every release artifact is fetched from. Matches `install.sh`
/// (`asset_url`/`sha256_url`) so the launcher and the shell bootstrap agree
/// on where a release lives.
pub const RELEASE_DOWNLOAD_ORIGIN: &str =
    "https://github.com/zeroclaw-labs/zeroclaw/releases/download";

/// Name of the checksum manifest published beside every release asset. It is
/// the launcher's only source of expected artifact digests.
pub const CHECKSUM_MANIFEST_ASSET: &str = "SHA256SUMS";

/// Maximum accepted tag length. Release tags are short; a long one is a sign
/// the input is not a tag.
const MAX_TAG_LEN: usize = 64;

/// A release tag that has passed the pinned-origin charset rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTag(String);

impl ReleaseTag {
    /// Validates a release tag.
    ///
    /// The charset is deliberately narrower than Git's: only ASCII
    /// alphanumerics, `.`, `-`, and `_`. That excludes `/`, `:`, `@`, `?`,
    /// `#`, and whitespace, which is what keeps a tag from re-pointing the
    /// URL at another host, another path, or a query string. `..` is rejected
    /// separately so a tag cannot walk up the release path.
    pub fn parse(raw: &str) -> Result<Self, BootstrapError> {
        let reject = |reason: &'static str| BootstrapError::MalformedReleaseTag {
            tag: raw.to_string(),
            reason,
        };

        if raw.is_empty() {
            return Err(reject("empty"));
        }
        if raw.len() > MAX_TAG_LEN {
            return Err(reject("longer than 64 characters"));
        }
        if raw.contains("..") {
            return Err(reject("contains a `..` path component"));
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(reject(
                "contains a character outside [A-Za-z0-9._-]; only a release tag is accepted here",
            ));
        }
        if raw.starts_with('-') || raw.starts_with('.') {
            return Err(reject("starts with `-` or `.`"));
        }
        Ok(Self(raw.to_string()))
    }

    /// The tag text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Product version implied by the tag, with any leading `v` stripped.
    pub fn version(&self) -> &str {
        self.0.strip_prefix('v').unwrap_or(&self.0)
    }
}

/// The default release tag: the version this launcher was built from.
///
/// The launcher deliberately does not resolve a moving "latest" pointer the
/// way `install.sh` and `src/commands/update.rs` do. A plan must name an
/// immutable release, because the human approves a specific artifact digest
/// and a moving pointer cannot be approved in advance.
pub fn default_release_tag() -> ReleaseTag {
    // The crate version is a compile-time constant of the accepted charset.
    ReleaseTag(format!("v{}", env!("CARGO_PKG_VERSION")))
}

/// A URL the launcher constructed from the pinned origin. Not constructible
/// from arbitrary text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedUrl(String);

impl PinnedUrl {
    /// Builds the URL of one asset in one release at the pinned origin.
    pub fn asset(tag: &ReleaseTag, asset_name: &str) -> Self {
        Self(format!(
            "{RELEASE_DOWNLOAD_ORIGIN}/{}/{asset_name}",
            tag.as_str()
        ))
    }

    /// Builds the URL of a release's checksum manifest.
    pub fn checksum_manifest(tag: &ReleaseTag) -> Self {
        Self::asset(tag, CHECKSUM_MANIFEST_ASSET)
    }

    /// The URL text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a URL lies under the pinned origin. Used to re-check a URL
    /// after redirects rather than trusting the transport.
    pub fn is_within_pinned_origin(candidate: &str) -> bool {
        candidate
            .strip_prefix(RELEASE_DOWNLOAD_ORIGIN)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_release_tags() {
        assert_eq!(
            ReleaseTag::parse("v0.8.4").expect("valid").as_str(),
            "v0.8.4"
        );
        assert_eq!(
            ReleaseTag::parse("v1.0.0-rc.1").expect("valid").version(),
            "1.0.0-rc.1"
        );
    }

    #[test]
    fn refuses_tags_that_could_repoint_the_origin() {
        for hostile in [
            "",
            "../../../evil",
            "v0.8.4/../../other",
            "https://evil.example/x",
            "v0.8.4?x=1",
            "v0.8.4#frag",
            "v0.8.4 extra",
            "v0.8.4/sub",
            "-v0.8.4",
            ".hidden",
        ] {
            assert!(
                ReleaseTag::parse(hostile).is_err(),
                "tag `{hostile}` must be refused"
            );
        }
    }

    #[test]
    fn refuses_overlong_tags() {
        assert!(ReleaseTag::parse(&"v".repeat(MAX_TAG_LEN + 1)).is_err());
    }

    #[test]
    fn pinned_urls_always_sit_under_the_pinned_origin() {
        let tag = ReleaseTag::parse("v0.8.4").expect("valid");
        let url = PinnedUrl::asset(&tag, "zeroclaw-x86_64-apple-darwin.tar.gz");
        assert_eq!(
            url.as_str(),
            "https://github.com/zeroclaw-labs/zeroclaw/releases/download/v0.8.4/zeroclaw-x86_64-apple-darwin.tar.gz"
        );
        assert!(PinnedUrl::is_within_pinned_origin(url.as_str()));
        assert!(PinnedUrl::is_within_pinned_origin(
            PinnedUrl::checksum_manifest(&tag).as_str()
        ));
    }

    #[test]
    fn origin_check_rejects_lookalike_hosts() {
        for hostile in [
            "https://evil.example/zeroclaw",
            "https://github.com/zeroclaw-labs/zeroclaw/releases/downloadEVIL/v1/a",
            "https://github.com.evil.example/zeroclaw-labs/zeroclaw/releases/download/v1/a",
            "http://github.com/zeroclaw-labs/zeroclaw/releases/download/v1/a",
        ] {
            assert!(
                !PinnedUrl::is_within_pinned_origin(hostile),
                "`{hostile}` must not count as the pinned origin"
            );
        }
    }
}
