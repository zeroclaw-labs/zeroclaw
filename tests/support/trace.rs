//! JSON trace fixture types — re-exported from the shipped `zeroclaw-eval` crate.
//!
//! These types were promoted out of this test-only module into `zeroclaw-eval`
//! (Phase 0 of the agent eval harness) so they ship as a product feature backing
//! `zeroclaw eval`. The re-export keeps existing test imports
//! (`super::trace::LlmTrace`, etc.) working unchanged.

pub use zeroclaw_eval::case::{
    LlmTrace, TraceExpects, TraceResponse, TraceStep, TraceToolCall, TraceTurn,
};
