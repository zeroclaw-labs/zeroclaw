//! Agent evaluation harness for ZeroClaw.
//!
//! **Phase 0 — deterministic replay.** This crate runs the *real* agent loop
//! ([`zeroclaw_runtime`]) against scripted LLM responses (an [`LlmTrace`] fixture)
//! and grades the outcome against declarative [`TraceExpects`]. Because the LLM
//! responses are fixed, a replay eval is free, fast, and fully deterministic —
//! it proves the agent *machinery* (tool parsing, dispatch, multi-turn looping)
//! behaves correctly given a known model output. It does **not** measure model
//! quality; that is the live mode added in later phases.
//!
//! ## Pieces
//! - [`case`] — the [`LlmTrace`] fixture/case format and suite loading.
//! - [`replay::TraceLlmProvider`] — a [`ModelProvider`](zeroclaw_api::model_provider::ModelProvider)
//!   that replays trace steps in FIFO order.
//! - [`tools`] — deterministic built-in tools the replay agent can dispatch.
//! - [`observer::RecordingObserver`] — captures tool-call names/outcomes and token
//!   usage from the agent run.
//! - [`grader`] — non-panicking [`GradeResult`](grader::GradeResult) checks over a run.
//! - [`runner`] — builds an isolated agent per case, drives it, and grades it.
//! - [`report`] — pass/fail aggregation and table/JSON rendering.

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
