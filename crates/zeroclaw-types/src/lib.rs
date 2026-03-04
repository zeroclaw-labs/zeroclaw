#![forbid(unsafe_code)]

//! Shared foundational types for the staged workspace split.
//!
//! This crate is intentionally minimal in PR-1 (scaffolding only).

/// Marker constant proving the crate is linked in workspace checks.
pub const CRATE_ID: &str = "zeroclaw-types";
