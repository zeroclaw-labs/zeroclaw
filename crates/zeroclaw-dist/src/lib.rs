//! Canonical distribution-target identity registry for ZeroClaw.
//!
//! This crate CREATES the canonical fact of which target triples ZeroClaw
//! publishes release archives for, and the identity each published target
//! carries: archive kind, primary binary name, release tier, and broad
//! platform family. The first bound consumers are the stable release workflow
//! matrix, its required-assets check, `install.sh`, `setup.bat`, the
//! self-updater, and distribution-exclusion metadata. They are parity-checked
//! here while later slices migrate them to direct generated consumption.
//! Additional target-naming surfaces must migrate or gain an explicit parity
//! contract before relying on this registry.
//!
//! The crate is deliberately behavior-free: identity data plus pure lookups
//! only. It carries no installer policy, download URLs, runner labels, cross
//! or linker settings, or feature selection. Parity between this registry and
//! the consuming surfaces is enforced by `tests/parity.rs`.

/// Archive format a target's release artifact is published as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// Gzip-compressed tarball (`.tar.gz`), used on Unix-family targets.
    TarGz,
    /// Zip archive (`.zip`), used on Windows targets.
    Zip,
}

impl ArchiveKind {
    /// File extension of the published archive, without a leading dot.
    pub const fn extension(self) -> &'static str {
        match self {
            ArchiveKind::TarGz => "tar.gz",
            ArchiveKind::Zip => "zip",
        }
    }
}

/// Whether a target's archive must exist for a stable release to publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseTier {
    /// The release workflow refuses to publish without this target's archive.
    Required,
    /// Built on an allowed-to-fail matrix leg; the archive is attached when
    /// it builds but never blocks publishing.
    Experimental,
}

/// Broad platform family of a target, without toolchain or libc detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformFamily {
    /// Linux hosts (glibc and musl alike).
    Linux,
    /// Apple macOS hosts.
    MacOs,
    /// Microsoft Windows hosts.
    Windows,
    /// Android hosts (including Termux).
    Android,
}

/// Identity of one published distribution target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistTarget {
    /// Rust target triple the release binary is built for.
    pub triple: &'static str,
    /// Archive format the target's release artifact is published as.
    pub archive: ArchiveKind,
    /// File name of the primary binary inside the archive.
    pub binary_name: &'static str,
    /// Whether the archive is required for a stable release to publish.
    pub tier: ReleaseTier,
    /// Broad platform family the target belongs to.
    pub family: PlatformFamily,
}

/// Every target the stable release workflow builds, in matrix order.
pub static DIST_TARGETS: [DistTarget; 10] = [
    DistTarget {
        triple: "x86_64-unknown-linux-gnu",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::Linux,
    },
    DistTarget {
        triple: "x86_64-unknown-linux-musl",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::Linux,
    },
    DistTarget {
        triple: "aarch64-unknown-linux-gnu",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::Linux,
    },
    DistTarget {
        triple: "aarch64-unknown-linux-musl",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::Linux,
    },
    DistTarget {
        triple: "armv7-unknown-linux-gnueabihf",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::Linux,
    },
    DistTarget {
        triple: "arm-unknown-linux-gnueabihf",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::Linux,
    },
    DistTarget {
        triple: "aarch64-apple-darwin",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::MacOs,
    },
    DistTarget {
        triple: "x86_64-apple-darwin",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Required,
        family: PlatformFamily::MacOs,
    },
    DistTarget {
        triple: "aarch64-linux-android",
        archive: ArchiveKind::TarGz,
        binary_name: "zeroclaw",
        tier: ReleaseTier::Experimental,
        family: PlatformFamily::Android,
    },
    DistTarget {
        triple: "x86_64-pc-windows-msvc",
        archive: ArchiveKind::Zip,
        binary_name: "zeroclaw.exe",
        tier: ReleaseTier::Required,
        family: PlatformFamily::Windows,
    },
];

/// Looks up a registered distribution target by its exact triple.
pub const fn find_target(triple: &str) -> Option<&'static DistTarget> {
    let mut i = 0;
    while i < DIST_TARGETS.len() {
        if str_eq(DIST_TARGETS[i].triple, triple) {
            return Some(&DIST_TARGETS[i]);
        }
        i += 1;
    }
    None
}

/// Targets whose archives must exist for a stable release to publish.
pub fn required_targets() -> impl Iterator<Item = &'static DistTarget> {
    DIST_TARGETS
        .iter()
        .filter(|target| target.tier == ReleaseTier::Required)
}

/// Targets built on allowed-to-fail legs and attached only when they build.
pub fn experimental_targets() -> impl Iterator<Item = &'static DistTarget> {
    DIST_TARGETS
        .iter()
        .filter(|target| target.tier == ReleaseTier::Experimental)
}

// Byte-wise equality because `==` on `&str` is not callable in const fn.
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn triples_are_unique() {
        let unique: BTreeSet<&str> = DIST_TARGETS.iter().map(|t| t.triple).collect();
        assert_eq!(unique.len(), DIST_TARGETS.len());
    }

    #[test]
    fn find_target_round_trips_every_registered_triple() {
        for target in &DIST_TARGETS {
            let found = find_target(target.triple).expect("registered triple must resolve");
            assert_eq!(found, target);
        }
    }

    #[test]
    fn find_target_rejects_unknown_triples() {
        assert!(find_target("riscv64gc-unknown-linux-gnu").is_none());
        assert!(find_target("").is_none());
        assert!(find_target("x86_64-unknown-linux-gnu ").is_none());
    }

    #[test]
    fn tier_iterators_partition_the_registry() {
        assert_eq!(
            required_targets().count() + experimental_targets().count(),
            DIST_TARGETS.len()
        );
        assert!(required_targets().all(|t| t.tier == ReleaseTier::Required));
        assert!(experimental_targets().all(|t| t.tier == ReleaseTier::Experimental));
    }
}
