#![forbid(unsafe_code)]

//! Core contracts for the staged workspace split.
//!
//! This crate is intentionally minimal in PR-1 (scaffolding only).

/// Marker constant proving dependency linkage to `zeroclaw-types`.
pub const CORE_CRATE_ID: &str = zeroclaw_types::CRATE_ID;
