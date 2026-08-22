//! Plan selection: one artifact, generated from the canonical registry, or a
//! refusal.

mod support;

use support::{FixtureOrigin, NeverFetches, checksum_manifest};

use zeroclaw_bootstrap::error::BootstrapError;
use zeroclaw_bootstrap::origin::{PinnedUrl, ReleaseTag};
use zeroclaw_bootstrap::plan::{HostEnv, InstallPlan};

fn unix_env() -> HostEnv {
    HostEnv {
        cargo_home: Some(std::path::PathBuf::from("/fixture/cargo")),
        home: Some(std::path::PathBuf::from("/fixture/home")),
        user_profile: Some(std::path::PathBuf::from(r"C:\fixture\user")),
    }
}

fn tag() -> ReleaseTag {
    ReleaseTag::parse("v0.8.4").expect("valid tag")
}

/// Builds an origin carrying a manifest that lists every registered target's
/// asset, so any registry target can be planned against it.
fn origin_with_full_manifest() -> (FixtureOrigin, Vec<(String, Vec<u8>)>) {
    let bodies: Vec<(String, Vec<u8>)> = zeroclaw_dist::DIST_TARGETS
        .iter()
        .map(|target| {
            let asset = format!("zeroclaw-{}.{}", target.triple, target.archive.extension());
            let body = format!("archive bytes for {}", target.triple).into_bytes();
            (asset, body)
        })
        .collect();
    let entries: Vec<(&str, &[u8])> = bodies
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_slice()))
        .collect();
    let manifest = checksum_manifest(&entries);

    let mut origin =
        FixtureOrigin::new().with(&PinnedUrl::checksum_manifest(&tag()), manifest.into_bytes());
    for (asset, body) in &bodies {
        origin = origin.with(&PinnedUrl::asset(&tag(), asset), body.clone());
    }
    (origin, bodies)
}

#[test]
fn plans_the_registry_artifact_for_every_published_target() {
    let (origin, _) = origin_with_full_manifest();
    let env = unix_env();

    for target in &zeroclaw_dist::DIST_TARGETS {
        let plan = InstallPlan::resolve(&origin, &env, target.triple, tag())
            .unwrap_or_else(|err| panic!("{} must plan: {err}", target.triple));

        assert_eq!(plan.target.triple, target.triple);
        assert_eq!(
            plan.asset_name,
            format!("zeroclaw-{}.{}", target.triple, target.archive.extension()),
            "asset name must be generated from the registry"
        );
        assert_eq!(
            plan.source_url.as_str(),
            format!(
                "https://github.com/zeroclaw-labs/zeroclaw/releases/download/v0.8.4/{}",
                plan.asset_name
            )
        );
        assert!(PinnedUrl::is_within_pinned_origin(plan.source_url.as_str()));
        assert!(PinnedUrl::is_within_pinned_origin(
            plan.manifest_url.as_str()
        ));
        assert_eq!(plan.artifact_digest.len(), 64);
        assert_eq!(plan.version, "0.8.4");
        assert!(
            plan.binary_path.ends_with(target.binary_name),
            "binary name must come from the registry"
        );
    }
}

#[test]
fn selects_the_platform_correct_install_location() {
    let (origin, _) = origin_with_full_manifest();
    let env = unix_env();

    let linux = InstallPlan::resolve(&origin, &env, "x86_64-unknown-linux-gnu", tag())
        .expect("linux target plans");
    assert_eq!(
        linux.binary_path,
        std::path::PathBuf::from("/fixture/cargo/bin/zeroclaw")
    );

    let windows = InstallPlan::resolve(&origin, &env, "x86_64-pc-windows-msvc", tag())
        .expect("windows target plans");
    assert_eq!(
        windows.binary_path,
        std::path::PathBuf::from(r"C:\fixture\user")
            .join(".zeroclaw")
            .join("bin")
            .join("zeroclaw.exe")
    );
}

#[test]
fn a_musl_host_never_plans_a_glibc_archive() {
    // `src/commands/update.rs` mis-resolves musl hosts onto glibc archives
    // today (see zeroclaw-dist's KNOWN_UPDATE_MISSING_TARGETS). The launcher
    // resolves the exact triple or refuses.
    let (origin, _) = origin_with_full_manifest();
    let plan = InstallPlan::resolve(&origin, &unix_env(), "x86_64-unknown-linux-musl", tag())
        .expect("musl target plans");
    assert_eq!(plan.target.triple, "x86_64-unknown-linux-musl");
    assert!(plan.asset_name.contains("musl"));
}

#[test]
fn refuses_an_unpublished_target_without_fetching_anything() {
    for unsupported in [
        "riscv64gc-unknown-linux-gnu",
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-gnu",
        "x86_64-unknown-linux-gnu ",
        "",
    ] {
        let err = InstallPlan::resolve(&NeverFetches, &unix_env(), unsupported, tag())
            .expect_err("unsupported target must be refused");
        assert!(
            matches!(err, BootstrapError::UnsupportedTarget { .. }),
            "`{unsupported}` produced {err:?}"
        );
    }
}

#[test]
fn refuses_a_target_the_release_did_not_publish() {
    // A registered target whose asset is absent from that release's manifest.
    let origin = FixtureOrigin::new().with(
        &PinnedUrl::checksum_manifest(&tag()),
        checksum_manifest(&[("zeroclaw-x86_64-apple-darwin.tar.gz", b"body")]).into_bytes(),
    );
    let err = InstallPlan::resolve(&origin, &unix_env(), "aarch64-linux-android", tag())
        .expect_err("unpublished asset must be refused");
    assert!(matches!(err, BootstrapError::ArtifactNotPublished { .. }));
}

#[test]
fn the_plan_digest_covers_every_security_relevant_fact() {
    let (origin, _) = origin_with_full_manifest();
    let base = InstallPlan::resolve(&origin, &unix_env(), "x86_64-unknown-linux-gnu", tag())
        .expect("plans");

    // Same inputs, same token.
    let repeat = InstallPlan::resolve(&origin, &unix_env(), "x86_64-unknown-linux-gnu", tag())
        .expect("plans");
    assert_eq!(base.digest(), repeat.digest());

    // A different target is a different approval.
    let other = InstallPlan::resolve(&origin, &unix_env(), "aarch64-unknown-linux-gnu", tag())
        .expect("plans");
    assert_ne!(base.digest(), other.digest());

    // A different install root is a different approval.
    let moved_env = HostEnv {
        cargo_home: Some(std::path::PathBuf::from("/elsewhere/cargo")),
        ..unix_env()
    };
    let moved = InstallPlan::resolve(&origin, &moved_env, "x86_64-unknown-linux-gnu", tag())
        .expect("plans");
    assert_ne!(base.digest(), moved.digest());

    // A different artifact digest is a different approval.
    let republished_manifest = checksum_manifest(&[(
        "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
        b"different bytes",
    )]);
    let republished = FixtureOrigin::new().with(
        &PinnedUrl::checksum_manifest(&tag()),
        republished_manifest.into_bytes(),
    );
    let republished_plan =
        InstallPlan::resolve(&republished, &unix_env(), "x86_64-unknown-linux-gnu", tag())
            .expect("plans");
    assert_ne!(base.digest(), republished_plan.digest());

    assert!(base.digest().starts_with("sha256:"));
}

#[test]
fn the_rendered_plan_shows_everything_an_approval_covers() {
    let (origin, _) = origin_with_full_manifest();
    let plan = InstallPlan::resolve(&origin, &unix_env(), "x86_64-unknown-linux-gnu", tag())
        .expect("plans");
    let rendered = plan.render();

    for expected in [
        "0.8.4",
        "stable",
        "v0.8.4",
        "x86_64-unknown-linux-gnu",
        "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
        "https://github.com/zeroclaw-labs/zeroclaw/releases/download/",
        &plan.artifact_digest,
        "NOT verified by this launcher",
        "/fixture/cargo/bin",
        "none (per-user install directory)",
        &plan.digest(),
    ] {
        assert!(
            rendered.contains(expected),
            "plan output must disclose {expected:?}:\n{rendered}"
        );
    }
}

#[test]
fn refuses_a_tag_that_could_repoint_the_origin() {
    for hostile in [
        "../../evil",
        "v1/../../x",
        "https://evil.example/x",
        "v1 v2",
    ] {
        assert!(
            ReleaseTag::parse(hostile).is_err(),
            "tag `{hostile}` must be refused before any URL is built"
        );
    }
}
