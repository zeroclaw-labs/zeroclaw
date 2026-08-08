//! The CI gate: every fixture in evals/regression must replay green.

use std::path::PathBuf;
use zeroclaw_config::scattered_types::EvalHarnessConfig;
use zeroclaw_eval::{Mode, run_suite};

/// Resolve the gated suite from the shipped config default rather than a second
/// hardcoded literal, so the directory this gate certifies cannot drift away
/// from the directory `zeroclaw eval run` uses by default.
fn regression_dir() -> PathBuf {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    repo_root.join(EvalHarnessConfig::default().suite_dir)
}

#[tokio::test]
async fn regression_suite_replays_green() {
    let report = run_suite(&regression_dir(), Mode::Replay)
        .await
        .expect("regression suite must load and run");
    assert!(
        report.all_passed(),
        "regression suite failed:\n{}",
        report.render_table()
    );
    assert_eq!(report.exit_code(), 0);
}

#[test]
fn gated_suite_directory_matches_the_configured_default() {
    assert_eq!(
        EvalHarnessConfig::default().suite_dir,
        "evals/regression",
        "the CI gate certifies the configured default suite; if this default moves, \
         move the gated fixtures with it"
    );
    assert!(
        regression_dir().is_dir(),
        "configured default suite directory must exist at {}",
        regression_dir().display()
    );
}
