//! The CI gate: every fixture in evals/regression must replay green.

use std::path::PathBuf;
use zeroclaw_eval::{Mode, run_suite};

fn regression_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/regression")
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
