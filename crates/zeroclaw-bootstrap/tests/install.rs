//! Install: the approval binding, the digest gate, and where bytes may land.

mod support;

use support::{FixtureOrigin, NeverFetches, checksum_manifest, tar_gz};

use zeroclaw_bootstrap::error::BootstrapError;
use zeroclaw_bootstrap::install;
use zeroclaw_bootstrap::origin::{PinnedUrl, ReleaseTag};
use zeroclaw_bootstrap::plan::{HostEnv, InstallPlan};

const TRIPLE: &str = "x86_64-unknown-linux-gnu";
const ASSET: &str = "zeroclaw-x86_64-unknown-linux-gnu.tar.gz";
const BINARY_BODY: &[u8] = b"#!/bin/sh\necho 'zeroclaw 0.8.4'\n";

fn tag() -> ReleaseTag {
    ReleaseTag::parse("v0.8.4").expect("valid tag")
}

struct Fixture {
    _root: tempfile::TempDir,
    env: HostEnv,
    origin: FixtureOrigin,
    archive: Vec<u8>,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("temp root");
    let cargo_home = root.path().join("cargo");
    let env = HostEnv {
        cargo_home: Some(cargo_home),
        home: Some(root.path().join("home")),
        user_profile: Some(root.path().join("profile")),
    };

    let archive = tar_gz(&[
        ("zeroclaw", BINARY_BODY),
        ("web/dist/index.html", b"<html>"),
    ]);
    let manifest = checksum_manifest(&[(ASSET, &archive)]);
    let origin = FixtureOrigin::new()
        .with(&PinnedUrl::checksum_manifest(&tag()), manifest.into_bytes())
        .with(&PinnedUrl::asset(&tag(), ASSET), archive.clone());

    Fixture {
        _root: root,
        env,
        origin,
        archive,
    }
}

fn plan_for(fixture: &Fixture) -> InstallPlan {
    InstallPlan::resolve(&fixture.origin, &fixture.env, TRIPLE, tag()).expect("plans")
}

#[test]
fn installs_the_approved_artifact_under_the_approved_location_only() {
    let fixture = fixture();
    let plan = plan_for(&fixture);
    let approval = plan.digest();

    let outcome = install::install(&fixture.origin, &plan, Some(&approval)).expect("installs");

    assert_eq!(outcome.binary_path, plan.binary_path);
    assert!(
        plan.binary_path.exists(),
        "binary must exist at the approved path"
    );
    assert_eq!(
        std::fs::read(&plan.binary_path).expect("read installed binary"),
        BINARY_BODY,
        "the installed bytes must be the archive's primary binary"
    );

    // Nothing may be written outside the approved install directory. The
    // archive also carried `web/dist/index.html`; the launcher installs only
    // the registry-named binary.
    let entries: Vec<_> = std::fs::read_dir(&plan.install_dir)
        .expect("install dir exists")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("zeroclaw")]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&plan.binary_path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "installed binary must be executable");
    }
}

#[test]
fn refuses_an_install_with_no_approval_token() {
    let fixture = fixture();
    let plan = plan_for(&fixture);

    // NeverFetches proves the refusal happens before any download.
    let err = install::install(&NeverFetches, &plan, None).expect_err("must refuse");
    assert!(matches!(err, BootstrapError::ApprovalMissing), "{err:?}");
    assert!(!plan.install_dir.exists(), "nothing may be created");
}

#[test]
fn refuses_an_approval_token_for_a_different_plan() {
    let fixture = fixture();
    let plan = plan_for(&fixture);

    let other_env = HostEnv {
        cargo_home: Some(fixture._root.path().join("other-cargo")),
        ..fixture.env.clone()
    };
    let other_plan = InstallPlan::resolve(&fixture.origin, &other_env, TRIPLE, tag())
        .expect("other plan resolves");
    assert_ne!(plan.digest(), other_plan.digest());

    let err = install::install(&NeverFetches, &plan, Some(&other_plan.digest()))
        .expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::ApprovalMismatch { .. }),
        "{err:?}"
    );
    assert!(!plan.install_dir.exists(), "nothing may be created");
}

#[test]
fn refuses_a_fabricated_approval_token() {
    let fixture = fixture();
    let plan = plan_for(&fixture);

    for fabricated in [
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "approved",
        "true",
        "",
    ] {
        let err =
            install::install(&NeverFetches, &plan, Some(fabricated)).expect_err("must refuse");
        assert!(
            matches!(err, BootstrapError::ApprovalMismatch { .. }),
            "`{fabricated}` produced {err:?}"
        );
    }
    assert!(!plan.install_dir.exists());
}

#[test]
fn refuses_an_artifact_whose_digest_does_not_match_the_approved_plan() {
    let fixture = fixture();
    let plan = plan_for(&fixture);
    let approval = plan.digest();

    // The origin serves different bytes than the manifest promised — a
    // republished or tampered artifact.
    let tampered = FixtureOrigin::new()
        .with(
            &PinnedUrl::checksum_manifest(&tag()),
            checksum_manifest(&[(ASSET, &fixture.archive)]).into_bytes(),
        )
        .with(
            &PinnedUrl::asset(&tag(), ASSET),
            tar_gz(&[("zeroclaw", b"malicious payload")]),
        );

    let err = install::install(&tampered, &plan, Some(&approval)).expect_err("must refuse");
    match err {
        BootstrapError::DigestMismatch { expected, actual } => {
            assert_eq!(expected, plan.artifact_digest);
            assert_ne!(actual, expected);
        }
        other => panic!("expected a digest refusal, got {other:?}"),
    }

    assert!(
        !plan.binary_path.exists(),
        "no byte may reach the install path when the digest does not match"
    );
}

#[test]
fn refuses_an_archive_without_the_registry_named_binary() {
    let fixture = fixture();
    let archive = tar_gz(&[("something-else", b"payload")]);
    let manifest = checksum_manifest(&[(ASSET, &archive)]);
    let origin = FixtureOrigin::new()
        .with(&PinnedUrl::checksum_manifest(&tag()), manifest.into_bytes())
        .with(&PinnedUrl::asset(&tag(), ASSET), archive);

    let plan = InstallPlan::resolve(&origin, &fixture.env, TRIPLE, tag()).expect("plans");
    let err = install::install(&origin, &plan, Some(&plan.digest())).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::BinaryMissingFromArchive { .. }),
        "{err:?}"
    );
    assert!(!plan.binary_path.exists());
}

#[test]
fn refuses_an_archive_entry_that_would_escape_the_install_directory() {
    let fixture = fixture();
    let archive = support::tar_gz_with_raw_names(&[
        ("../../../etc/cron.d/evil", b"payload"),
        ("zeroclaw", BINARY_BODY),
    ]);
    let manifest = checksum_manifest(&[(ASSET, &archive)]);
    let origin = FixtureOrigin::new()
        .with(&PinnedUrl::checksum_manifest(&tag()), manifest.into_bytes())
        .with(&PinnedUrl::asset(&tag(), ASSET), archive);

    let plan = InstallPlan::resolve(&origin, &fixture.env, TRIPLE, tag()).expect("plans");
    let err = install::install(&origin, &plan, Some(&plan.digest())).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::UnsafeArchiveEntry { .. }),
        "{err:?}"
    );
    assert!(!plan.binary_path.exists());
}

#[test]
fn a_nested_binary_does_not_stand_in_for_the_top_level_one() {
    let fixture = fixture();
    let archive = tar_gz(&[("nested/zeroclaw", b"impostor")]);
    let manifest = checksum_manifest(&[(ASSET, &archive)]);
    let origin = FixtureOrigin::new()
        .with(&PinnedUrl::checksum_manifest(&tag()), manifest.into_bytes())
        .with(&PinnedUrl::asset(&tag(), ASSET), archive);

    let plan = InstallPlan::resolve(&origin, &fixture.env, TRIPLE, tag()).expect("plans");
    let err = install::install(&origin, &plan, Some(&plan.digest())).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::BinaryMissingFromArchive { .. }),
        "{err:?}"
    );
}

#[test]
fn the_approval_check_is_independently_enforced() {
    let fixture = fixture();
    let plan = plan_for(&fixture);

    assert!(install::check_approval(&plan, None).is_err());
    assert!(install::check_approval(&plan, Some("nonsense")).is_err());
    assert!(install::check_approval(&plan, Some(&plan.digest())).is_ok());
    // Copy-pasted tokens often carry whitespace; that is the same decision.
    assert!(install::check_approval(&plan, Some(&format!("  {}\n", plan.digest()))).is_ok());
}
