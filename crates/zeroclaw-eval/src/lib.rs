//! Agent evaluation harness for ZeroClaw.

pub mod case;
pub mod grader;
pub mod observer;
pub mod record;
pub mod replay;
pub mod report;
pub mod runner;
pub mod tools;

pub use case::{LlmTrace, TraceExpects};
pub use record::RunRecord;
pub use report::{CaseReport, SuiteReport};
pub use runner::{run_case, run_suite};

use std::str::FromStr;

/// How an evaluation suite is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Deterministic replay against scripted LLM responses — no network, no cost.
    Replay,
    /// Live execution against a real provider. Added in a later phase; the Phase 0
    /// runner returns a clear error so the variant can already be parsed from the CLI.
    Live,
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "replay" => Ok(Mode::Replay),
            "live" => Ok(Mode::Live),
            other => anyhow::bail!("unknown eval mode '{other}' (expected 'replay' or 'live')"),
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Mode::Replay => "replay",
            Mode::Live => "live",
        })
    }
}
