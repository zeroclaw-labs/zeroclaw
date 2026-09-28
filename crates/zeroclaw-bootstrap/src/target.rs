//! Host target resolution and artifact naming, generated from the canonical
//! distribution registry.
//!
//! This module creates no facts. Which triples exist, which archive format
//! each publishes, and what the primary binary inside is called are all read
//! from `zeroclaw_dist::DIST_TARGETS`. Adding a release target to the registry
//! makes it selectable here with no edit in this crate; removing one makes it
//! unselectable. There is deliberately no local target table to drift.

use zeroclaw_dist::{DistTarget, PlatformFamily, ReleaseTier, find_target};

use crate::error::BootstrapError;

/// The exact target triple this launcher binary was built for, recorded by
/// `build.rs` from Cargo's own `TARGET`.
pub const HOST_TARGET: &str = env!("ZEROCLAW_BOOTSTRAP_HOST_TARGET");

/// Every triple the canonical registry publishes an archive for, in matrix
/// order.
pub fn published_triples() -> Vec<&'static str> {
    zeroclaw_dist::DIST_TARGETS
        .iter()
        .map(|t| t.triple)
        .collect()
}

/// Resolves a triple to its registry entry, refusing anything unpublished.
///
/// This is the only way a target enters a plan. There is no "closest match"
/// and no fallback to a related triple: a musl host never resolves to a glibc
/// archive here.
pub fn resolve(triple: &str) -> Result<&'static DistTarget, BootstrapError> {
    find_target(triple).ok_or_else(|| BootstrapError::UnsupportedTarget {
        detected: triple.to_string(),
        supported: published_triples(),
    })
}

/// Release asset name for a target, generated from the registry exactly as
/// the release workflow and `crates/zeroclaw-dist/tests/parity.rs` do.
pub fn asset_name(target: &DistTarget) -> String {
    format!("zeroclaw-{}.{}", target.triple, target.archive.extension())
}

/// Human-readable release tier, for plan output.
pub fn tier_label(tier: ReleaseTier) -> &'static str {
    match tier {
        ReleaseTier::Required => "required",
        ReleaseTier::Experimental => "experimental (allowed-to-fail build leg)",
    }
}

/// Human-readable platform family, for status and plan output.
pub fn family_label(family: PlatformFamily) -> &'static str {
    match family {
        PlatformFamily::Linux => "linux",
        PlatformFamily::MacOs => "macos",
        PlatformFamily::Windows => "windows",
        PlatformFamily::Android => "android",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registry_target_resolves_and_names_an_asset() {
        for target in &zeroclaw_dist::DIST_TARGETS {
            let resolved = resolve(target.triple).expect("registry target must resolve");
            assert_eq!(resolved.triple, target.triple);
            assert_eq!(
                asset_name(resolved),
                format!("zeroclaw-{}.{}", target.triple, target.archive.extension())
            );
        }
    }

    #[test]
    fn published_triples_is_the_registry_and_nothing_else() {
        assert_eq!(published_triples().len(), zeroclaw_dist::DIST_TARGETS.len());
    }

    #[test]
    fn host_target_is_recorded_by_the_build_script() {
        assert!(!HOST_TARGET.is_empty());
        assert!(HOST_TARGET.contains('-'));
    }
}
