//! Agent evaluation harness for ZeroClaw.

pub mod baseline;
pub mod case;
pub mod grader;
pub mod live;
pub mod observer;
pub mod record;
pub mod replay;
pub mod report;
pub mod runner;
pub mod tools;

pub use case::{CaseSetup, LlmTrace, TraceExpects};
pub use grader::{GradeCategory, GradeContext, GradeResult, Grader};
pub use record::RunRecord;
pub use report::{CaseReport, SuiteReport};
pub use runner::{CaseOutcome, CaseProvider, RunDeps, ensure_live_provider, run_case, run_suite};

use std::str::FromStr;

/// How an evaluation suite is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Deterministic replay against scripted LLM responses — no network, no cost.
    Replay,
    /// Live execution against the provider configured in `[eval].live_provider`.
    /// Makes real network calls and incurs provider cost; each case runs in an
    /// isolated sandboxed workspace under the configured tool allowlist.
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
