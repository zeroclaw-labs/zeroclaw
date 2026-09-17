//! Typed refusals.
//!
//! Every failure mode of the launcher is a named variant. There is no
//! catch-all "something went wrong" that a caller could mistake for a
//! recoverable condition, and no variant carries a fallback: a refusal ends
//! the operation. `install` in particular never degrades to "install anyway"
//! for any variant here.

use std::fmt;
use std::path::PathBuf;

/// A refusal or hard failure from a bootstrap operation.
#[derive(Debug)]
pub enum BootstrapError {
    /// The host triple is not one of the published release targets.
    UnsupportedTarget {
        /// Triple the host resolved to.
        detected: String,
        /// Every triple the canonical registry publishes an archive for.
        supported: Vec<&'static str>,
    },
    /// A release tag failed the pinned-origin charset rule. Rejecting these
    /// is what keeps the download origin pinned: a tag containing `/`, `..`,
    /// or a scheme could otherwise re-point the URL at another host.
    MalformedReleaseTag {
        /// The rejected tag, verbatim.
        tag: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// `install` was invoked without the approval token that `plan` prints.
    ApprovalMissing,
    /// The supplied approval token does not match the plan recomputed now.
    /// The plan changed between `plan` and `install`, or the token is not
    /// this plan's.
    ApprovalMismatch {
        /// Digest of the plan as recomputed at install time.
        expected: String,
        /// Digest the caller supplied.
        provided: String,
    },
    /// The checksum manifest at the pinned origin has no entry for the
    /// artifact this plan selected.
    ArtifactNotPublished {
        /// Asset name that has no manifest entry.
        asset: String,
        /// Release tag whose manifest was consulted.
        tag: String,
    },
    /// Downloaded bytes do not hash to the digest the approved plan pinned.
    /// Nothing is written to the install path on this path.
    DigestMismatch {
        /// Digest recorded in the approved plan.
        expected: String,
        /// Digest of the bytes actually received.
        actual: String,
    },
    /// A checksum-manifest line could not be parsed as `<digest>  <name>`.
    MalformedChecksumManifest {
        /// Why parsing failed.
        reason: String,
    },
    /// An archive entry would escape the install directory (absolute path,
    /// `..` component, or a symlink/link entry).
    UnsafeArchiveEntry {
        /// The rejected entry path, verbatim.
        entry: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// The archive does not contain the primary binary the registry names.
    BinaryMissingFromArchive {
        /// Binary name the registry records for this target.
        expected: String,
    },
    /// The install root could not be derived from the host environment.
    /// The launcher accepts no install-root argument, so an underivable root
    /// is a refusal rather than a prompt.
    InstallRootUnresolvable {
        /// Environment variables that were consulted and found unusable.
        reason: String,
    },
    /// The server advertised a control protocol whose major version the
    /// launcher does not support. Fails closed; never downgrades.
    UnsupportedProtocolVersion {
        /// Version string the server advertised.
        advertised: String,
        /// Range this launcher accepts.
        supported: String,
    },
    /// The `initialize` result is missing a required advertisement field.
    AdvertisementIncomplete {
        /// The missing or malformed field.
        field: &'static str,
    },
    /// The advertised product version is not the version that was installed
    /// and verified.
    ProductVersionMismatch {
        /// Version the launcher verified on disk.
        expected: String,
        /// Version the server advertised.
        advertised: String,
    },
    /// The executable that answered `initialize` is not the artifact the
    /// launcher verified.
    ExecutableIdentityMismatch {
        /// Digest of the binary the launcher verified and installed.
        expected: String,
        /// Digest of the binary actually present at handoff time.
        actual: String,
    },
    /// The handoff target does not exist or is not executable.
    HandoffTargetUnusable {
        /// Path that was probed.
        path: PathBuf,
        /// Why it is unusable.
        reason: String,
    },
    /// The control server produced no parsable `initialize` response.
    HandoffProbeFailed {
        /// What went wrong during the probe.
        reason: String,
    },
    /// Network transport failure against the pinned origin.
    Transport {
        /// The pinned URL that failed. Always launcher-constructed.
        url: String,
        /// Underlying failure text.
        reason: String,
    },
    /// Filesystem failure.
    Io {
        /// What the launcher was doing.
        context: String,
        /// Underlying failure text.
        reason: String,
    },
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget {
                detected,
                supported,
            } => write!(
                f,
                "refused: no published release artifact for host target `{detected}`; \
                 published targets are: {}",
                supported.join(", ")
            ),
            Self::MalformedReleaseTag { tag, reason } => write!(
                f,
                "refused: release tag `{tag}` is not acceptable ({reason}); \
                 the launcher accepts no download URL, so a tag may only name a release"
            ),
            Self::ApprovalMissing => write!(
                f,
                "refused: install requires --approve <plan-digest>; run `plan` and pass the \
                 plan digest it prints. A model cannot satisfy this by asserting approval"
            ),
            Self::ApprovalMismatch { expected, provided } => write!(
                f,
                "refused: approval token does not match the current plan\n  \
                 plan now:  {expected}\n  approved:  {provided}\n  \
                 the plan changed since it was approved; re-run `plan` and review it again"
            ),
            Self::ArtifactNotPublished { asset, tag } => write!(
                f,
                "refused: release `{tag}` publishes no checksum entry for `{asset}`"
            ),
            Self::DigestMismatch { expected, actual } => write!(
                f,
                "refused: artifact digest mismatch; nothing was written to the install path\n  \
                 expected: {expected}\n  actual:   {actual}"
            ),
            Self::MalformedChecksumManifest { reason } => {
                write!(f, "refused: checksum manifest is malformed ({reason})")
            }
            Self::UnsafeArchiveEntry { entry, reason } => {
                write!(f, "refused: archive entry `{entry}` rejected ({reason})")
            }
            Self::BinaryMissingFromArchive { expected } => write!(
                f,
                "refused: archive does not contain the expected binary `{expected}`"
            ),
            Self::InstallRootUnresolvable { reason } => write!(
                f,
                "refused: cannot derive the approved install root ({reason}); \
                 the launcher accepts no install-root argument"
            ),
            Self::UnsupportedProtocolVersion {
                advertised,
                supported,
            } => write!(
                f,
                "refused: server advertises control protocol `{advertised}`, \
                 this launcher supports {supported}"
            ),
            Self::AdvertisementIncomplete { field } => write!(
                f,
                "refused: initialize advertisement is missing or malformed at `{field}`"
            ),
            Self::ProductVersionMismatch {
                expected,
                advertised,
            } => write!(
                f,
                "refused: verified binary is version {expected} but the server advertises \
                 {advertised}"
            ),
            Self::ExecutableIdentityMismatch { expected, actual } => write!(
                f,
                "refused: the binary at the handoff path is not the artifact this launcher \
                 verified\n  expected: {expected}\n  actual:   {actual}"
            ),
            Self::HandoffTargetUnusable { path, reason } => write!(
                f,
                "refused: handoff target {} is unusable ({reason})",
                path.display()
            ),
            Self::HandoffProbeFailed { reason } => {
                write!(f, "refused: control server probe failed ({reason})")
            }
            Self::Transport { url, reason } => {
                write!(f, "failed: request to {url} did not complete ({reason})")
            }
            Self::Io { context, reason } => write!(f, "failed: {context} ({reason})"),
        }
    }
}

impl std::error::Error for BootstrapError {}

impl BootstrapError {
    /// Builds an [`BootstrapError::Io`] from a context string and a source.
    pub(crate) fn io(context: impl Into<String>, source: &dyn fmt::Display) -> Self {
        Self::Io {
            context: context.into(),
            reason: source.to_string(),
        }
    }
}
