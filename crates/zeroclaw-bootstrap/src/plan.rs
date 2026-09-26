//! The install plan and its approval digest.
//!
//! A plan is the complete, reviewable description of one install: which
//! artifact, from where, with which digest, to which path, at what privilege.
//! Its digest covers every one of those facts, so approving the digest is
//! approving exactly that install and nothing else. If any fact changes
//! between `plan` and `install` — a re-pointed tag, a republished artifact, a
//! different install root in the environment — the recomputed digest differs
//! and the approval no longer matches.

use std::path::PathBuf;

use zeroclaw_dist::{DistTarget, PlatformFamily};

use crate::error::BootstrapError;
use crate::fetch::{Fetcher, expected_digest_for};
use crate::origin::{PinnedUrl, ReleaseTag};
use crate::target;

/// Release channel. Only the stable channel publishes the archives the
/// canonical registry describes, so there is no channel argument to get
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel;

impl Channel {
    /// Channel label for plan output.
    pub const LABEL: &'static str = "stable";
}

/// What the launcher can say about release signatures today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureStatus {
    /// The release carries GitHub-hosted SLSA provenance attestations, which
    /// this launcher does not verify. Verifying them needs a Sigstore
    /// verifier the launcher deliberately does not carry, so it reports the
    /// exact command a human can run instead of implying a check it did not
    /// perform.
    AttestedNotVerifiedByLauncher {
        /// Command that verifies the artifact out of band.
        verify_command: String,
    },
}

impl SignatureStatus {
    fn for_asset(asset_name: &str) -> Self {
        Self::AttestedNotVerifiedByLauncher {
            verify_command: format!(
                "gh attestation verify {asset_name} --repo zeroclaw-labs/zeroclaw \
                 --signer-workflow zeroclaw-labs/zeroclaw/.github/workflows/release-stable-manual.yml"
            ),
        }
    }

    /// One-line summary for plan output.
    pub fn summary(&self) -> &'static str {
        match self {
            Self::AttestedNotVerifiedByLauncher { .. } => {
                "SLSA provenance published; NOT verified by this launcher"
            }
        }
    }
}

/// Privilege the install path requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    /// Writable by the invoking user; no elevation.
    User,
}

impl Privilege {
    /// Label for plan output.
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "none (per-user install directory)",
        }
    }
}

/// The environment facts an install root is derived from.
///
/// Passed in rather than read from the process so the derivation is testable
/// without mutating global state. The launcher's own binary builds this from
/// `std::env`; nothing else can point it elsewhere, and whatever it resolves
/// to is printed in the plan and bound into the approval digest.
#[derive(Debug, Clone, Default)]
pub struct HostEnv {
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// `$CARGO_HOME`.
    pub cargo_home: Option<PathBuf>,
    /// `%USERPROFILE%`.
    pub user_profile: Option<PathBuf>,
}

impl HostEnv {
    /// Reads the environment variables the install root is derived from.
    pub fn from_process() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            cargo_home: std::env::var_os("CARGO_HOME").map(PathBuf::from),
            user_profile: std::env::var_os("USERPROFILE").map(PathBuf::from),
        }
    }

    /// Derives the one approved install directory for a platform family.
    ///
    /// Unix families use `$CARGO_HOME/bin` (default `$HOME/.cargo/bin`),
    /// matching where `install.sh` places a prebuilt binary. Windows uses
    /// `%USERPROFILE%\.zeroclaw\bin`, matching `setup.bat`. Both are per-user
    /// directories, which is why no elevation is ever requested.
    pub fn install_dir(&self, family: PlatformFamily) -> Result<PathBuf, BootstrapError> {
        match family {
            PlatformFamily::Windows => self
                .user_profile
                .clone()
                .map(|profile| profile.join(".zeroclaw").join("bin"))
                .ok_or_else(|| BootstrapError::InstallRootUnresolvable {
                    reason: "USERPROFILE is not set".to_string(),
                }),
            _ => self
                .cargo_home
                .clone()
                .or_else(|| self.home.as_ref().map(|home| home.join(".cargo")))
                .map(|cargo_home| cargo_home.join("bin"))
                .ok_or_else(|| BootstrapError::InstallRootUnresolvable {
                    reason: "neither CARGO_HOME nor HOME is set".to_string(),
                }),
        }
    }
}

/// One reviewable, approvable install.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    /// Release tag the artifact belongs to.
    pub tag: ReleaseTag,
    /// Product version implied by the tag.
    pub version: String,
    /// Registry entry the plan selected.
    pub target: &'static DistTarget,
    /// Asset name, generated from the registry.
    pub asset_name: String,
    /// Artifact URL at the pinned origin.
    pub source_url: PinnedUrl,
    /// Checksum-manifest URL at the pinned origin.
    pub manifest_url: PinnedUrl,
    /// Expected artifact digest, read from the release's checksum manifest.
    pub artifact_digest: String,
    /// What the launcher can and cannot say about signatures.
    pub signature: SignatureStatus,
    /// Directory the binary is installed into.
    pub install_dir: PathBuf,
    /// Full path of the installed binary.
    pub binary_path: PathBuf,
    /// Privilege the install requires.
    pub privilege: Privilege,
}

impl InstallPlan {
    /// Builds the plan for one target and release.
    ///
    /// The expected artifact digest is read from the release's `SHA256SUMS`
    /// before any artifact is fetched, so a target with no published archive
    /// is refused without downloading anything.
    pub fn resolve(
        fetcher: &dyn Fetcher,
        env: &HostEnv,
        triple: &str,
        tag: ReleaseTag,
    ) -> Result<Self, BootstrapError> {
        let target = target::resolve(triple)?;
        let asset_name = target::asset_name(target);
        let manifest_url = PinnedUrl::checksum_manifest(&tag);

        let manifest_bytes = fetcher.fetch(&manifest_url)?;
        let manifest = String::from_utf8(manifest_bytes).map_err(|err| {
            BootstrapError::MalformedChecksumManifest {
                reason: format!("manifest is not UTF-8 ({err})"),
            }
        })?;
        let artifact_digest = expected_digest_for(&manifest, &asset_name).map_err(|_| {
            BootstrapError::ArtifactNotPublished {
                asset: asset_name.clone(),
                tag: tag.as_str().to_string(),
            }
        })?;

        let install_dir = env.install_dir(target.family)?;
        let binary_path = install_dir.join(target.binary_name);

        Ok(Self {
            version: tag.version().to_string(),
            source_url: PinnedUrl::asset(&tag, &asset_name),
            signature: SignatureStatus::for_asset(&asset_name),
            tag,
            target,
            asset_name,
            manifest_url,
            artifact_digest,
            install_dir,
            binary_path,
            privilege: Privilege::User,
        })
    }

    /// Canonical text the approval digest is taken over.
    ///
    /// Field order is fixed and every security-relevant fact appears exactly
    /// once. Anything omitted here would be a fact the human did not approve,
    /// so new plan fields must be added to this function.
    pub fn canonical_form(&self) -> String {
        let mut out = String::new();
        let mut push = |key: &str, value: &str| {
            out.push_str(key);
            out.push('\x1f');
            out.push_str(value);
            out.push('\x1e');
        };
        push("plan-version", "1");
        push("channel", Channel::LABEL);
        push("tag", self.tag.as_str());
        push("version", &self.version);
        push("triple", self.target.triple);
        push("asset", &self.asset_name);
        push("source-url", self.source_url.as_str());
        push("manifest-url", self.manifest_url.as_str());
        push("artifact-sha256", &self.artifact_digest);
        push("binary-name", self.target.binary_name);
        push("install-dir", &self.install_dir.to_string_lossy());
        push("binary-path", &self.binary_path.to_string_lossy());
        push("privilege", self.privilege.label());
        push("signature", self.signature.summary());
        out
    }

    /// The approval token for this exact plan.
    pub fn digest(&self) -> String {
        format!(
            "sha256:{}",
            crate::fetch::sha256_hex(self.canonical_form().as_bytes())
        )
    }

    /// Human-readable plan, as printed by `plan` and re-printed by `install`.
    pub fn render(&self) -> String {
        let SignatureStatus::AttestedNotVerifiedByLauncher { verify_command } = &self.signature;
        format!(
            "Install plan\n\
             \x20 version           {}\n\
             \x20 channel           {}\n\
             \x20 release tag       {}\n\
             \x20 target            {} ({}, {})\n\
             \x20 artifact          {}\n\
             \x20 source            {}\n\
             \x20 digest source     {}\n\
             \x20 artifact sha256   {}\n\
             \x20 signature         {}\n\
             \x20                   verify out of band with:\n\
             \x20                     {}\n\
             \x20 install dir       {}\n\
             \x20 binary path       {}\n\
             \x20 privilege         {}\n\
             \n\
             Approve this exact plan by passing its digest to `install`:\n\
             \x20 --approve {}\n",
            self.version,
            Channel::LABEL,
            self.tag.as_str(),
            self.target.triple,
            target::family_label(self.target.family),
            target::tier_label(self.target.tier),
            self.asset_name,
            self.source_url.as_str(),
            self.manifest_url.as_str(),
            self.artifact_digest,
            self.signature.summary(),
            verify_command,
            self.install_dir.display(),
            self.binary_path.display(),
            self.privilege.label(),
            self.digest(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_install_dir_prefers_cargo_home_then_home() {
        let env = HostEnv {
            cargo_home: Some(PathBuf::from("/opt/cargo")),
            home: Some(PathBuf::from("/home/u")),
            user_profile: None,
        };
        assert_eq!(
            env.install_dir(PlatformFamily::Linux).expect("resolves"),
            PathBuf::from("/opt/cargo/bin")
        );

        let env = HostEnv {
            cargo_home: None,
            home: Some(PathBuf::from("/home/u")),
            user_profile: None,
        };
        assert_eq!(
            env.install_dir(PlatformFamily::MacOs).expect("resolves"),
            PathBuf::from("/home/u/.cargo/bin")
        );
    }

    #[test]
    fn windows_install_dir_uses_the_user_profile() {
        let env = HostEnv {
            user_profile: Some(PathBuf::from(r"C:\Users\u")),
            ..HostEnv::default()
        };
        assert_eq!(
            env.install_dir(PlatformFamily::Windows).expect("resolves"),
            PathBuf::from(r"C:\Users\u").join(".zeroclaw").join("bin")
        );
    }

    #[test]
    fn an_underivable_install_root_is_refused_not_guessed() {
        let env = HostEnv::default();
        assert!(matches!(
            env.install_dir(PlatformFamily::Linux),
            Err(BootstrapError::InstallRootUnresolvable { .. })
        ));
        assert!(matches!(
            env.install_dir(PlatformFamily::Windows),
            Err(BootstrapError::InstallRootUnresolvable { .. })
        ));
    }
}
