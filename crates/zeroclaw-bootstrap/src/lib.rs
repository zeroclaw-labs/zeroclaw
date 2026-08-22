//! Bootstrap install launcher for ZeroClaw.
//!
//! An MCP server inside the ZeroClaw binary cannot install that binary when it
//! is absent. This crate is the small distribution client that closes that
//! gap: an external harness runs it to identify the host, review an install,
//! install exactly the artifact a human approved, and hand off to
//! `zeroclaw control --mcp`. It is a distribution client, not a second
//! configuration service.
//!
//! This module set is the foundation: typed refusals, the pinned release
//! origin, target resolution generated from the canonical distribution
//! registry, and digest-verified fetching.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod fetch;
pub mod origin;
pub mod target;

pub use error::BootstrapError;
