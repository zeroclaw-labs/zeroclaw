//! Non-fatal validation warnings — config that loads and validates
//! successfully (i.e. `Config::validate()` returns `Ok(())`) but will fail
//! at agent runtime because of a logical inconsistency the schema can't
//! enforce structurally.
//!
//! The CLI surfaces these via `tracing::warn!` so operators see them on
//! stderr. The gateway HTTP API surfaces them via the `warnings` field on
//! `PropResponse` / `PatchResponse` so dashboard callers see the same
//! signal — closing the parity gap that previously left a dashboard user
//! with no indication their config would fail at runtime.
//!
//! Each warning carries:
//! - a stable `code` (machine-friendly, matches across releases for a
//!   given check)
//! - a human-readable `message` (suitable for direct display to operators)
//! - the dotted property `path` the warning concerns (so the dashboard
//!   can highlight the offending field)
//!
//! Adding a new warning: append the check to `Config::collect_warnings`
//! in `schema.rs` and pick a stable `code`. `Config::validate` emits each
//! collected warning via `tracing::warn!` so logs continue to show them.

use serde::{Deserialize, Serialize};

/// One non-fatal validation issue surfaced after a successful save.
///
/// Stable codes (extend as new warnings are added):
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ValidationWarning {
    /// Stable machine-readable identifier for the warning class.
    pub code: String,
    /// Human-readable description suitable for direct display.
    pub message: String,
    /// Dotted property path the warning concerns
    /// (e.g. `"providers.fallback"`).
    pub path: String,
}

impl ValidationWarning {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: path.into(),
        }
    }
}
