//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::Result;
use std::path::PathBuf;
use zeroclaw_eval::{Mode, SuiteReport};

/// Run a suite of eval cases and return the aggregated report.
pub async fn run(suite: PathBuf, mode: Mode) -> Result<SuiteReport> {
    Box::pin(zeroclaw_eval::run_suite(&suite, mode)).await
}

/// Output format for the eval report.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table.
    Table,
    /// Machine-readable JSON, for CI artifacts.
    Json,
}

/// Render a suite report in the requested format.
pub fn print_report(report: &SuiteReport, format: OutputFormat) {
    match format {
        OutputFormat::Json => println!("{}", report.to_json()),
        OutputFormat::Table => println!("{}", report.render_table()),
    }
}
