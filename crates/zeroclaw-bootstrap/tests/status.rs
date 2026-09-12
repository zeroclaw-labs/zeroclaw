//! Status: what the launcher can say about a host without changing it.

mod support;

use zeroclaw_bootstrap::plan::HostEnv;
use zeroclaw_bootstrap::status::{self, BinaryState, Recommendation};

fn env_rooted_at(root: &std::path::Path) -> HostEnv {
    HostEnv {
        cargo_home: Some(root.join("cargo")),
        home: Some(root.join("home")),
        user_profile: Some(root.join("profile")),
    }
}

#[test]
fn reports_an_absent_binary_and_recommends_planning() {
    let root = tempfile::tempdir().expect("temp root");
    let report = status::status(&env_rooted_at(root.path()), "x86_64-unknown-linux-gnu");

    assert_eq!(report.binary, BinaryState::Absent);
    assert_eq!(report.recommendation, Recommendation::PlanInstall);
    assert!(report.unsupported.is_none());
    assert_eq!(
        report.target.expect("published target").triple,
        "x86_64-unknown-linux-gnu"
    );

    // The absent branch routes the harness through the full install path and
    // names `configure` as the destination, with a machine-readable token.
    let rendered = report.render();
    assert!(rendered.contains("existing binary   none"));
    assert!(
        rendered.contains("next action       install"),
        "absent status must emit the machine-readable install token:\n{rendered}"
    );
    assert!(
        rendered.contains("ZeroClaw is not installed"),
        "absent status must state the instance is not installed:\n{rendered}"
    );
    for step in ["plan", "install", "handoff", "configure"] {
        assert!(
            rendered.contains(step),
            "absent status must name `{step}` in the route:\n{rendered}"
        );
    }
}

#[test]
fn refuses_an_unpublished_host_without_inventing_a_target() {
    let root = tempfile::tempdir().expect("temp root");
    let report = status::status(&env_rooted_at(root.path()), "riscv64gc-unknown-linux-gnu");

    assert!(report.target.is_none());
    assert!(
        report
            .unsupported
            .as_deref()
            .is_some_and(|text| text.contains("no published release artifact")),
        "status must state the refusal: {:?}",
        report.unsupported
    );
    assert!(report.render().contains("published         no"));
}

#[cfg(unix)]
#[test]
fn verifies_a_binary_that_reports_a_zeroclaw_version() {
    let root = tempfile::tempdir().expect("temp root");
    let env = env_rooted_at(root.path());
    let bin_dir = root.path().join("cargo").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    support::write_executable(
        &bin_dir,
        "zeroclaw",
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'zeroclaw 0.8.4'; exit 0; fi\nexit 1\n",
    );

    let report = status::status(&env, "x86_64-unknown-linux-gnu");
    match &report.binary {
        BinaryState::Verified {
            version, digest, ..
        } => {
            assert_eq!(version, "0.8.4");
            assert_eq!(digest.len(), 64);
        }
        other => panic!("expected a verified binary, got {other:?}"),
    }
    assert_eq!(report.recommendation, Recommendation::ReadyForHandoff);

    // The installed branch routes straight to `handoff` and names `configure`
    // as the destination, with a machine-readable token.
    let rendered = report.render();
    assert!(rendered.contains("0.8.4 (verified)"));
    assert!(
        rendered.contains("next action       configure"),
        "installed status must emit the machine-readable configure token:\n{rendered}"
    );
    assert!(
        rendered.contains("ZeroClaw is installed"),
        "installed status must state the instance is installed:\n{rendered}"
    );
    for step in ["handoff", "configure"] {
        assert!(
            rendered.contains(step),
            "installed status must name `{step}` in the route:\n{rendered}"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_unverifiable_binary_yields_a_repair_recommendation_not_a_replacement() {
    let root = tempfile::tempdir().expect("temp root");
    let env = env_rooted_at(root.path());
    let bin_dir = root.path().join("cargo").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");

    // A file that exists, runs, and is emphatically not ZeroClaw.
    let path = support::write_executable(
        &bin_dir,
        "zeroclaw",
        "#!/bin/sh\necho 'totally-other-tool 9.9.9'\nexit 0\n",
    );
    let before = std::fs::read(&path).expect("read fixture");

    let report = status::status(&env, "x86_64-unknown-linux-gnu");
    match &report.binary {
        BinaryState::Unverifiable { digest, reason, .. } => {
            assert!(
                digest.is_some(),
                "an existing file's digest is still reported"
            );
            assert!(
                reason.contains("no recognisable version banner"),
                "reason was: {reason}"
            );
        }
        other => panic!("expected an unverifiable binary, got {other:?}"),
    }
    assert_eq!(report.recommendation, Recommendation::PlanRepair);
    assert!(report.render().contains("UNVERIFIABLE"));

    assert_eq!(
        std::fs::read(&path).expect("read fixture"),
        before,
        "status must never replace an existing binary"
    );
}

#[cfg(unix)]
#[test]
fn a_binary_that_cannot_run_is_unverifiable_rather_than_absent() {
    let root = tempfile::tempdir().expect("temp root");
    let env = env_rooted_at(root.path());
    let bin_dir = root.path().join("cargo").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    // Present but not executable.
    std::fs::write(bin_dir.join("zeroclaw"), b"not an executable").expect("write");

    let report = status::status(&env, "x86_64-unknown-linux-gnu");
    assert!(
        matches!(report.binary, BinaryState::Unverifiable { .. }),
        "got {:?}",
        report.binary
    );
    assert_eq!(report.recommendation, Recommendation::PlanRepair);
}

#[cfg(unix)]
#[test]
fn a_binary_whose_version_exits_non_zero_is_unverifiable() {
    let root = tempfile::tempdir().expect("temp root");
    let env = env_rooted_at(root.path());
    let bin_dir = root.path().join("cargo").join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    support::write_executable(&bin_dir, "zeroclaw", "#!/bin/sh\nexit 3\n");

    let report = status::status(&env, "x86_64-unknown-linux-gnu");
    match &report.binary {
        BinaryState::Unverifiable { reason, .. } => {
            assert!(reason.contains("--version"), "reason was: {reason}");
        }
        other => panic!("expected unverifiable, got {other:?}"),
    }
}

#[test]
fn windows_status_looks_in_the_windows_install_location() {
    let root = tempfile::tempdir().expect("temp root");
    let report = status::status(&env_rooted_at(root.path()), "x86_64-pc-windows-msvc");

    let expected = root.path().join("profile").join(".zeroclaw").join("bin");
    assert_eq!(report.install_dir.as_deref(), Some(expected.as_path()));
    assert_eq!(
        report.target.expect("published target").binary_name,
        "zeroclaw.exe"
    );
}
