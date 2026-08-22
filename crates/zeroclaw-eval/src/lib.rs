//! Agent evaluation harness for ZeroClaw.

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
pub use grader::{
    GradeCategory, GradeContext, GradeResult, Grader, RunCompletedGrader, default_graders,
};
pub use record::RunRecord;
pub use report::{CaseReport, SuiteReport};
pub use runner::{
    CaseOutcome, CaseProvider, RunDeps, ensure_live_provider, run_case, run_case_with_graders,
    run_suite,
};

use std::str::FromStr;

/// How an evaluation suite is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Deterministic replay against scripted LLM responses — no network, no cost.
    Replay,
    /// Live execution against a real provider: real tokens, real network egress,
    /// non-deterministic output. The provider comes from `[eval] live_provider`,
    /// each turn is bounded by `[eval] case_timeout_secs`, the tool surface is the
    /// `[eval] live_allowed_tools` allowlist, and `shell` is hard-denied regardless.
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
