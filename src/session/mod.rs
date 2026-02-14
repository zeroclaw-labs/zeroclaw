//! Session management — transcript persistence, message types, repair, and usage tracking.
//!
//! Sessions are stored as JSONL files with a header line followed by message lines.
//! A session store (sessions.json) indexes all sessions with metadata.

pub mod types;
pub mod transcript;
pub mod repair;

pub use types::*;
