//! `zeroclaw eval` — run the agent evaluation harness.
//!
//! Phase 0 supports deterministic replay: each `*.json` trace fixture in the suite
//! directory is replayed through the real agent loop and graded against its
//! declarative expectations. The command exits non-zero if any case fails, so it
//! can gate CI.

use anyhow::Result;
use std::path::PathBuf;
use zeroclaw_eval::{Mode, SuiteReport};

/// Run a suite of eval cases and return the aggregated report.
pub async fn run(suite: PathBuf, mode: Mode) -> Result<SuiteReport> {
    zeroclaw_eval::run_suite(&suite, mode).await
}

/// Render a suite report in the requested format (`table` or `json`).
pub fn print_report(report: &SuiteReport, format: &str) {
    match format {
        "json" => println!("{}", report.to_json()),
        _ => println!("{}", report.render_table()),
    }
}
