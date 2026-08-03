//! Architecture invariant: **every channel a user can enable in config must be
//! reachable from a documented feature set.**
//!
//! # The failure this prevents
//!
//! `channels.whatsapp` is configured in TOML and gated behind the `whatsapp-web`
//! cargo feature — which is NOT in `default`. Building without it yields a
//! binary that starts perfectly: the service reports `active`, `zeroclaw doctor`
//! reports zero errors, the config is untouched and still says the channel is
//! enabled. The only symptom is the daemon logging
//!
//! ```text
//! No active channels to supervise (none configured or all disabled).
//! ```
//!
//! which points the operator at the *config* while the actual cause is a
//! *compile-time* feature that was never passed. An agent serving a real person
//! goes silent and every health surface says it is fine.
//!
//! # What is asserted
//!
//! 1. Every channel feature declared in `Cargo.toml` belongs to at least one
//!    aggregate feature set (`default-channels`, `channels-full`, or `ci-all`),
//!    so CI compiles it and no channel can rot unbuilt.
//! 2. Feature sets referenced here actually exist — the test cannot silently
//!    pass because a set was renamed.
//!
//! This is a source-level invariant, so it runs on any host without building a
//! release artifact. The complementary runtime check (does the *built binary*
//! really contain the channel) lives in `scripts/deploy-local.sh`, which
//! refuses to install an artifact missing a channel the local config enables.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn workspace_manifest() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace Cargo.toml must be readable")
}

/// Extract the members of a feature-set array such as `ci-all = [ ... ]`.
///
/// Returns `None` when the set is not declared at all, which the caller treats
/// as a hard failure rather than an empty set — a renamed aggregate must break
/// this test loudly instead of vacuously passing.
fn feature_set(manifest: &str, name: &str) -> Option<BTreeSet<String>> {
    let start = manifest.find(&format!("\n{name} = ["))?;
    let rest = &manifest[start + 1..];
    let end = rest.find(']')?;
    Some(
        rest[..end]
            .split(['[', ',', '\n'])
            .filter_map(|tok| {
                let t = tok.trim().trim_matches('"').trim();
                (!t.is_empty() && !t.starts_with('#') && !t.contains('=')).then(|| t.to_string())
            })
            .collect(),
    )
}

/// All features whose name marks them as a messaging channel.
fn declared_channel_features(manifest: &str) -> BTreeSet<String> {
    let features_start = manifest
        .find("\n[features]")
        .expect("[features] section must exist");
    let section = &manifest[features_start..];

    section
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let name = line.split('=').next()?.trim();
            // `channel-*` is the convention; `whatsapp-web` predates it and is
            // named for the transport rather than the pattern, which is exactly
            // why it was easy to overlook when picking build features.
            let is_channel = name.starts_with("channel-") || name == "whatsapp-web";
            (is_channel && !name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Feature sets in the `zeroclaw-channels` sub-crate.
///
/// The workspace aggregate `channels-full` pulls in
/// `zeroclaw-channels/channels-full` as a passthrough, so a channel listed only
/// in the sub-crate's own aggregate IS compiled by CI. Ignoring this indirection
/// would make the test report false orphans.
fn subcrate_feature_set(name: &str) -> BTreeSet<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("crates/zeroclaw-channels/Cargo.toml"))
        .expect("zeroclaw-channels Cargo.toml must be readable");
    feature_set(&manifest, name).unwrap_or_default()
}

#[test]
fn every_channel_feature_is_covered_by_an_aggregate_feature_set() {
    let manifest = workspace_manifest();
    let channels = declared_channel_features(&manifest);
    assert!(
        !channels.is_empty(),
        "no channel features detected — the manifest parser is broken, not the manifest"
    );

    // These aggregates are what CI actually builds. A channel outside all of
    // them is a channel nobody ever compiles.
    let aggregates = ["default-channels", "channels-full", "ci-all"];
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for name in aggregates {
        let set = feature_set(&manifest, name).unwrap_or_else(|| {
            panic!(
                "feature set `{name}` is missing from Cargo.toml. If it was renamed, \
                 update this test — do not delete the check, or channels silently \
                 stop being built."
            )
        });
        covered.extend(set);
    }
    // Channels reached indirectly through `zeroclaw-channels/channels-full`
    // count as covered — the workspace aggregate pulls that set in wholesale.
    covered.extend(subcrate_feature_set("channels-full"));

    let orphans: Vec<&String> = channels.difference(&covered).collect();
    assert!(
        orphans.is_empty(),
        "these channel features belong to no aggregate feature set ({}): {orphans:?}\n\
         A channel outside every aggregate is never compiled in CI, so a build \
         that omits it looks completely healthy while the channel is dead.\n\
         Add each one to `channels-full` (or `ci-all` when it needs extra system \
         dependencies).",
        aggregates.join(", ")
    );
}

#[test]
fn whatsapp_web_is_compiled_by_ci() {
    // Regression lock for the specific outage: `whatsapp-web` is absent from
    // `default`, so only its presence in `ci-all` keeps it building at all.
    let manifest = workspace_manifest();
    let ci_all = feature_set(&manifest, "ci-all").expect("`ci-all` feature set must exist");

    assert!(
        ci_all.contains("whatsapp-web"),
        "`whatsapp-web` dropped out of `ci-all`. It is NOT in `default`, so CI \
         would stop compiling the WhatsApp channel entirely — and a binary built \
         without it starts clean, passes `doctor`, and serves nothing."
    );

    let default = feature_set(&manifest, "default").expect("`default` feature set must exist");
    assert!(
        !default.contains("whatsapp-web"),
        "`whatsapp-web` is now in `default`. That is fine, but this test and \
         scripts/deploy-local.sh document it as opt-in; update both so the next \
         person is not warned about a hazard that no longer exists."
    );
}
